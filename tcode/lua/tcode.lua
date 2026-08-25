local M = {}

-- Watch a file for changes using inotify (fs_event).
-- The file must already exist. Errors on failure.
-- @param filepath: Path to the file to watch
-- @param on_change: Callback invoked when the file changes
-- @return table with a stop() method to clean up
local function watch_file(filepath, on_change)
  local handle = vim.uv.new_fs_event()
  local ret, err_name, err_msg = handle:start(filepath, {}, vim.schedule_wrap(function(err, filename, events)
    if err then
      error('fs_event error on ' .. filepath .. ': ' .. err)
      return
    end
    on_change()
  end))

  if not ret then
    handle:close()
    error('failed to watch ' .. filepath .. ': ' .. (err_name or 'unknown'))
  end

  -- Check for any existing content
  on_change()

  return {
    stop = function()
      handle:stop()
      handle:close()
    end,
  }
end

-- Format a millisecond epoch timestamp as HH:MM:SS
local function format_time(ts_millis)
  if not ts_millis then return nil end
  return os.date('%H:%M:%S', math.floor(ts_millis / 1000))
end

-- Single-quote a value for safe shell interpolation: wraps it in single
-- quotes and escapes embedded quotes (' -> '\''), so wire-derived strings
-- (the exe path, session ids, tool call ids, conversation ids) can never be
-- interpreted as shell syntax by the system() shell-outs.
local function shquote(s)
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

-- Ensure a buffer is modifiable before writing to it.
-- Returns false if the buffer is invalid, so caller can bail out.
-- Note: caller is responsible for resetting modifiable = false when done.
local function ensure_buf_modifiable(buf)
  if not vim.api.nvim_buf_is_valid(buf) then return false end
  vim.bo[buf].modifiable = true
  return true
end

-- Run fn with the buffer temporarily modifiable, then restore the previous
-- modifiable state. The restore runs even when fn errors, then the error is
-- re-raised. Returns nil if the buffer is invalid.
--
-- Restoring the *prior* value (rather than hardcoding false) makes nested use
-- safe: callers already inside a modifiable window (e.g. the JSONL batch
-- render) see the window stay open, while top-level writers outside any window
-- (e.g. the `o` expand/collapse toggles) restore the read-only display
-- invariant.
local function with_modifiable(buf, fn)
  if not vim.api.nvim_buf_is_valid(buf) then return nil end
  local was_modifiable = vim.bo[buf].modifiable
  vim.bo[buf].modifiable = true
  local ok, result = pcall(fn)
  if vim.api.nvim_buf_is_valid(buf) then
    vim.bo[buf].modifiable = was_modifiable
  end
  if not ok then
    error(result, 0)
  end
  return result
end

-- Append complete lines to the buffer
local function append_lines(buf, lines)
  if not ensure_buf_modifiable(buf) then return end
  local line_count = vim.api.nvim_buf_line_count(buf)
  vim.api.nvim_buf_set_lines(buf, line_count, line_count, false, lines)
end

-- Tool output is wrapped in a long backtick-fenced code block to prevent
-- markdown/treesitter from interpreting partial HTML, XML, JSON, etc. as
-- markdown syntax. We use 10 backticks so tool output containing ``` won't
-- accidentally close the fence.
local TC_FENCE = '``````````'

--- Show a y/n confirmation popup at the cursor and execute callback on confirm.
local function confirm_popup(prompt, on_confirm)
  -- Remember the window and buffer we came from so we can restore after the popup
  local parent_win = vim.api.nvim_get_current_win()
  local parent_buf = vim.api.nvim_get_current_buf()

  local popup_buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_lines(popup_buf, 0, -1, false, { prompt })
  local width = #prompt + 4
  local popup_win = vim.api.nvim_open_win(popup_buf, true, {
    relative = 'cursor',
    row = 1,
    col = 0,
    width = width,
    height = 1,
    style = 'minimal',
    border = 'rounded',
    noautocmd = true,
  })

  local function close_popup()
    -- Suppress all autocmds during close to prevent LazyVim plugins
    -- (file explorers, completion, etc.) from hijacking the display window
    local saved_ei = vim.o.eventignore
    vim.o.eventignore = 'all'
    local ok, err = pcall(function()
      if vim.api.nvim_win_is_valid(popup_win) then
        vim.api.nvim_win_close(popup_win, true)
      end
      if vim.api.nvim_buf_is_valid(popup_buf) then
        vim.api.nvim_buf_delete(popup_buf, { force = true })
      end
      -- Restore the parent window/buffer in case plugins already switched it
      if vim.api.nvim_win_is_valid(parent_win) and vim.api.nvim_buf_is_valid(parent_buf) then
        vim.api.nvim_win_set_buf(parent_win, parent_buf)
        vim.api.nvim_set_current_win(parent_win)
      end
    end)
    vim.o.eventignore = saved_ei
    if not ok then vim.api.nvim_err_writeln('close_popup: ' .. tostring(err)) end
  end

  vim.keymap.set('n', 'y', function()
    close_popup()
    on_confirm()
  end, { buffer = popup_buf, nowait = true })
  vim.keymap.set('n', 'n', close_popup, { buffer = popup_buf, nowait = true })
  vim.keymap.set('n', 'q', close_popup, { buffer = popup_buf, nowait = true })
  vim.keymap.set('n', '<Esc>', close_popup, { buffer = popup_buf, nowait = true })
end

-- Replace every NUL byte (0x00) with the two-byte display escape '\0'
-- (backslash + '0'). Byte-level by design: the Lua pattern engine treats a
-- NUL as a C-string terminator, so pattern-based replacement of '\0' is not
-- reliable (`gsub('\0', ...)` sees an EMPTY pattern and inserts between every
-- character). The plain find fast path returns the input unchanged when no
-- NUL is present (the common case — no allocation).
local function escape_nul(s)
  if not s:find('\0', 1, true) then return s end
  local out = {}
  local pos = 1
  while true do
    local b = s:find('\0', pos, true)
    if not b then
      out[#out + 1] = s:sub(pos)
      break
    end
    out[#out + 1] = s:sub(pos, b - 1)
    out[#out + 1] = '\\0'
    pos = b + 1
  end
  return table.concat(out)
end

--- Insert text at the end of a specific row, supporting multi-line text.
local function insert_text_at(buf, row, text)
  -- A NUL byte is an internal line break to nvim's buffer API; escape it
  -- (\0 -> \\0) so a projected row never becomes multiple buffer rows and
  -- desyncs the row map. Display-only: the model keeps the raw bytes.
  text = escape_nul(tostring(text))
  local cur_line = vim.api.nvim_buf_get_lines(buf, row, row + 1, false)[1] or ''
  local lines = vim.split(text, '\n', { plain = true })
  vim.api.nvim_buf_set_text(buf, row, #cur_line, row, #cur_line, lines)
end

-- Force render-markdown.nvim to repaint this buffer NOW.
--
-- Why: render-markdown.nvim is what conceals fenced code block delimiters
-- (the `````````` lines) when our buffer's filetype is `tcode` and the
-- plugin is configured to handle it. Its update path runs through
-- Decorator:schedule which is a *trailing-edge* debounce — as long as
-- schedule() calls keep arriving faster than `config.debounce` ms apart,
-- the running flag stays true forever and only the FIRST callback in the
-- burst actually fires. During streaming this means most batches never
-- get re-rendered, leaving newly inserted fence rows on screen as raw
-- backticks until streaming pauses for >100ms or the user moves the
-- cursor in the display window.
--
-- The mitigation has two parts working together:
--   1. set_render_markdown_debounce(buf, 0) below removes the rate limit
--      for our specific buffer, so every schedule() call reaches the
--      callback path.
--   2. force_render_markdown(buf), called once per event batch from the
--      JSONL reader after all events in that batch have been applied,
--      kicks the plugin so it actually re-runs against the post-batch
--      buffer state.
--
-- Wrapped in pcall so users without render-markdown installed get a
-- silent no-op. We do not currently integrate with any other markdown
-- rendering plugin (markview.nvim, headlines.nvim, etc.) — see the
-- limitation note in setup_display.
local function force_render_markdown(buf)
  if not vim.api.nvim_buf_is_valid(buf) then return end
  pcall(function()
    require('render-markdown.api').render({ buf = buf })
  end)
end

-- Override render-markdown.nvim's debounce for a specific buffer by
-- mutating the cached buffer config object in place. Must be called
-- AFTER the plugin's FileType-driven attach has populated the cache for
-- this buffer (i.e. after `vim.bo[buf].filetype = 'tcode'`). See the
-- long-form explanation on force_render_markdown above for why this is
-- necessary. Silent no-op if render-markdown is not installed.
local function set_render_markdown_debounce(buf, ms)
  if not vim.api.nvim_buf_is_valid(buf) then return end
  pcall(function()
    local cfg = require('render-markdown.state').get(buf)
    if cfg then
      cfg.debounce = ms
    end
  end)
end

-- ============================================================================
-- MODEL
-- ============================================================================
-- Pure in-memory representation of the display pane: a flat ordered list of
-- elements plus the bookkeeping the reducer needs. The reducer is the only
-- writer; the renderer projects elements onto the buffer. This layer never
-- touches a buffer, an extmark, or vim.*.

-- Fresh empty model. `tail` is the element whose content is currently at the
-- buffer tail (nil when none); `sa_active` is the conversation_id of the
-- subagent whose output is streaming; `pending_whitespace` holds
-- whitespace-only assistant text awaiting flush (see the reducer);
-- `full_input` is set by the tool-call detail view so args are never collapsed.
local function new_model()
  return {
    elements = {},            -- ordered list of elements
    by_id = {},               -- id -> element
    tail = nil,               -- element whose content is at the buffer tail
    sa_active = nil,          -- conversation_id of the streaming subagent
    pending_whitespace = nil, -- whitespace-only text awaiting flush
    full_input = false,       -- detail view: never collapse tool args
    next_id = 0,              -- id mint counter
  }
end

local model = new_model()

-- Mint an id, register the element in by_id, append it to the ordered list.
-- Callers set model.tail explicitly after each transition.
local function add_element(model, element)
  model.next_id = model.next_id + 1
  element.id = model.next_id
  table.insert(model.elements, element)
  model.by_id[element.id] = element
  return element
end

-- ============================================================================
-- REDUCER
-- ============================================================================
-- Pure transition functions. apply(model, event, envelope_id) -> diff where
-- diff = { added = {element,...}, updated_all = {element,...},
--          updated_content = {{element, text}, ...} }. The diff IS the change
-- tracking: the reducer just mutated the model, so it tags exactly what it
-- did. `bulk` is not a reducer concern. No buffer / extmark / vim.* access.

local function new_diff()
  return { added = {}, updated_all = {}, updated_content = {} }
end

-- Append an updated_content entry, coalescing consecutive deltas for the same
-- element into one entry (the renderer appends per entry). The entry's text is
-- held as a PARTS LIST so a long streaming run never re-copies the accumulated
-- string (each `..` on a growing string is O(n); a 300k-chunk thinking run
-- would otherwise be quadratic). The renderer materializes with
-- updated_content_text before writing.
local function add_updated_content(diff, element, text)
  -- text may be a plain chunk or an existing parts list (merge_diff forwards
  -- frag entries verbatim); normalize to a parts list first.
  local parts
  if type(text) == 'table' then
    parts = text
  else
    parts = { text }
  end
  local uc = diff.updated_content
  local last = uc[#uc]
  if last and last[1] == element then
    local t = last[2]
    if type(t) ~= 'table' then
      t = { t }
      last[2] = t
    end
    for i = 1, #parts do t[#t + 1] = parts[i] end
  else
    uc[#uc + 1] = { element, parts }
  end
end

-- Materialize an updated_content entry's parts list into one string.
local function updated_content_text(entry)
  local t = entry[2]
  if type(t) == 'table' then return table.concat(t) end
  return t
end

-- Append a streaming chunk to an element's content field. The append is
-- amortized O(1): chunks accumulate in a parts list and are folded into the
-- string field by content_of on the next read. Without this, every chunk
-- re-copies the whole accumulated string (`x = x .. c` is quadratic in this
-- LuaJIT build) — a single long thinking stream would pin a core.
local function append_content(el, field, chunk)
  local parts = el[field .. '_parts']
  if not parts then
    parts = {}
    el[field .. '_parts'] = parts
  end
  parts[#parts + 1] = chunk
end

-- Materialize a streamed content field (fold pending parts into the string
-- field, clearing the parts list) and return the full text. Idempotent and
-- safe for whole-set fields (user content, tool_args fallback): when no parts
-- are pending it just returns the field. Reads go through this helper so the
-- model field is ALWAYS the complete string at the point of use.
local function content_of(el, field)
  local parts = el[field .. '_parts']
  if parts then
    local s = (el[field] or '') .. table.concat(parts)
    el[field] = s
    el[field .. '_parts'] = nil
    return s
  end
  return el[field] or ''
end

-- Merge a fragment diff (from a helper) into the main diff.
local function merge_diff(diff, frag)
  if not frag then return diff end
  for _, el in ipairs(frag.added) do
    diff.added[#diff.added + 1] = el
  end
  for _, el in ipairs(frag.updated_all) do
    diff.updated_all[#diff.updated_all + 1] = el
  end
  for _, entry in ipairs(frag.updated_content) do
    add_updated_content(diff, entry[1], entry[2])
  end
  return diff
end

-- True when the text splits on '\n' into only empty lines (a pure equivalent
-- of vim.split(text, '\n', {plain = true}) with every line empty): the chunk
-- is whitespace-only and belongs in pending_whitespace, not in the message.
local function is_whitespace_only(text)
  for i = 1, #text do
    if text:byte(i) ~= 10 then -- any char that is not '\n'
      return false
    end
  end
  return true
end

-- Number of lines the text splits into on '\n' (pure equivalent of
-- #vim.split(text, '\n', {plain = true})).
local function count_lines(text)
  if text == '' then return 1 end
  local n = 1
  for i = 1, #text do
    if text:byte(i) == 10 then
      n = n + 1
    end
  end
  return n
end

-- Last element of a given type in the model.
local function last_element_of_type(model, type_)
  for i = #model.elements, 1, -1 do
    local el = model.elements[i]
    if el.type == type_ then return el end
  end
  return nil
end

-- Lookup helpers. Scans run backwards so the most recently added element wins;
-- parallel tool calls / subagent sections may share a tool_call_index, hence
-- the type filter.
local function find_tool_call_by_id(model, tool_call_id)
  if not tool_call_id then return nil end
  local by = model.by_id[tool_call_id]
  if by and by.type == 'tool_call' then return by end
  for i = #model.elements, 1, -1 do
    local el = model.elements[i]
    if el.type == 'tool_call' and el.tool_call_id == tool_call_id then
      return el
    end
  end
  return nil
end

local function find_tool_call_by_index(model, tool_call_index)
  for i = #model.elements, 1, -1 do
    local el = model.elements[i]
    if el.type == 'tool_call' and el.tool_call_index == tool_call_index then
      return el
    end
  end
  return nil
end

local function find_subagent_input_by_index(model, tool_call_index)
  for i = #model.elements, 1, -1 do
    local el = model.elements[i]
    -- A pending subagent has conversation_id == nil until SubAgentStart /
    -- SubAgentContinue (exactly like find_pending_subagent).
    if el.type == 'subagent' and el.tool_call_index == tool_call_index
      and el.conversation_id == nil then
      return el
    end
  end
  return nil
end

-- A subagent awaiting SubAgentStart/SubAgentContinue has no conversation_id
-- yet. AssistantMessageEnd may already have closed its input fence, so the
-- pending test is conversation_id == nil, not input_open.
local function find_pending_subagent(model, tool_call_id)
  if not tool_call_id then return nil end
  for i = #model.elements, 1, -1 do
    local el = model.elements[i]
    if el.type == 'subagent' and el.tool_call_id == tool_call_id and el.conversation_id == nil then
      return el
    end
  end
  return nil
end

local function find_subagent_by_conversation(model, conversation_id)
  if not conversation_id then return nil end
  for i = #model.elements, 1, -1 do
    local el = model.elements[i]
    if el.type == 'subagent' and el.conversation_id == conversation_id then
      return el
    end
  end
  return nil
end

-- Flush held whitespace onto the (last) assistant message. When no assistant
-- message exists the whitespace has nowhere to land: discard it rather than
-- materialize a phantom '► ASSISTANT' block (a stray whitespace-only chunk
-- flushed at UserMessage / AssistantRequestEnd must not render). Returns a
-- diff fragment.
local function flush_pending_whitespace(model)
  local pending = model.pending_whitespace
  if not pending then return new_diff() end
  model.pending_whitespace = nil
  local am = last_element_of_type(model, 'assistant_message')
  local frag = new_diff()
  if not am then
    return frag -- discard: no assistant message to append to
  end
  append_content(am, 'content', pending)
  add_updated_content(frag, am, pending)
  return frag
end

-- Collapse the expanded thinking block (structurally always the tail) to
-- 'collapsed'. Returns a full diff, empty when the tail is not expanded.
local function collapse_open_thinking(model)
  local diff = new_diff()
  local tail = model.tail
  if tail and tail.type == 'thinking_block' and tail.state == 'expanded' then
    tail.state = 'collapsed'
    diff.updated_all[#diff.updated_all + 1] = tail
  end
  return diff
end

-- Reducer operation: close every open element (an expanded thinking tail,
-- open args/input fences), producing updated_all per element. No production
-- caller; kept as a test-facing reducer op.
local function close_open_elements(model)
  local diff = merge_diff(new_diff(), collapse_open_thinking(model))
  for _, el in ipairs(model.elements) do
    if el.type == 'tool_call' and el.args_open then
      el.args_open = false
      diff.updated_all[#diff.updated_all + 1] = el
    elseif el.type == 'subagent' and el.input_open then
      el.input_open = false
      diff.updated_all[#diff.updated_all + 1] = el
    end
  end
  return diff
end

-- `o` toggle on a thinking block: collapsed <-> expanded.
local function toggle_thinking_element(model, element)
  local diff = new_diff()
  if element and element.type == 'thinking_block'
    and (element.state == 'collapsed' or element.state == 'expanded') then
    element.state = element.state == 'collapsed' and 'expanded' or 'collapsed'
    diff.updated_all[#diff.updated_all + 1] = element
  end
  return diff
end

-- Apply one display event (an unwrapped msg table) to the model and return the
-- diff. Events arrive in wire order; the model appends in event order.
local function apply(model, event, envelope_id)
  local diff = new_diff()
  if not event then return diff end
  local variant, data = next(event)
  if not variant then return diff end
  data = data or {}

  if variant == 'UserMessage' then
    merge_diff(diff, flush_pending_whitespace(model))
    merge_diff(diff, collapse_open_thinking(model))
    local el = add_element(model, {
      type = 'user_message',
      msg_id = envelope_id or data.msg_id,
      content = data.content,
      media_filenames = data.media_filenames,
      created_at = data.created_at,
    })
    diff.added[#diff.added + 1] = el
    model.tail = el

  elseif variant == 'AssistantMessageStart' then
    merge_diff(diff, flush_pending_whitespace(model))
    merge_diff(diff, collapse_open_thinking(model))
    local el = add_element(model, {
      type = 'assistant_message',
      content = '',
      created_at = data.created_at,
    })
    diff.added[#diff.added + 1] = el
    model.tail = el

  elseif variant == 'AssistantThinkingChunk' then
    local chunk = data.content or ''
    if chunk == '' then return diff end -- never create/erase on an empty chunk
    local tail = model.tail
    if tail and tail.type == 'thinking_block' then
      -- Streaming into the tail thinking block, collapsed or expanded.
      -- Content accumulates in the model either way; the updated_content
      -- entry (and the visible stream) appears only while expanded. A
      -- collapsed block accumulates invisibly and rebuilds in full when
      -- toggled expanded.
      append_content(tail, 'content', chunk)
      model.pending_whitespace = nil
      if tail.state == 'expanded' then
        add_updated_content(diff, tail, chunk)
      end
    else
      -- New run: collapse any expanded thinking first, then open a fresh
      -- expanded block. A run that starts while the model tail is an
      -- assistant_message is ATTACHED to that am: the renderer places its
      -- rows INSIDE the am's region at the arrival position, so the display
      -- order matches the wire order (thinking before response).
      merge_diff(diff, collapse_open_thinking(model))
      local el = add_element(model, {
        type = 'thinking_block',
        content = chunk,
        state = 'expanded',
        attach_to = tail and tail.type == 'assistant_message' and tail.id or nil,
      })
      diff.added[#diff.added + 1] = el
      model.tail = el
    end

  elseif variant == 'AssistantMessageChunk' then
    local chunk = data.content or ''
    -- Empty chunk: a true no-op (empty diff) - no collapse, no tail move.
    if chunk == '' then return diff end
    if model.sa_active then
      -- Subagent output streams through AssistantMessageChunk after
      -- SubAgentStart: append to the active subagent's output, not the
      -- assistant message. Runs before the whitespace handling so
      -- whitespace during subagent streaming stays subagent output.
      local sa = find_subagent_by_conversation(model, model.sa_active)
      if sa then
        append_content(sa, 'output', chunk)
        add_updated_content(diff, sa, chunk)
        return diff -- tail unchanged
      end
    end
    if is_whitespace_only(chunk) then
      -- Hold whitespace-only text: no collapse, no tail move (it would
      -- otherwise fold a streaming expanded thinking tail). It flushes
      -- prepended to the next real text chunk.
      model.pending_whitespace = (model.pending_whitespace or '') .. chunk
      return diff -- no diff entries for the chunk itself
    end
    -- Real text: collapse any expanded thinking tail, then flush held
    -- whitespace prepended to this chunk.
    merge_diff(diff, collapse_open_thinking(model))
    merge_diff(diff, flush_pending_whitespace(model))
    local am = last_element_of_type(model, 'assistant_message')
    if not am then
      -- Defensive: AssistantMessageStart normally precedes, but a bare text
      -- chunk must still land somewhere.
      am = add_element(model, { type = 'assistant_message', content = '' })
      diff.added[#diff.added + 1] = am
    end
    append_content(am, 'content', chunk)
    add_updated_content(diff, am, chunk)
    model.tail = am

  elseif variant == 'AssistantMessageEnd' then
    merge_diff(diff, flush_pending_whitespace(model))
    -- Close every still-open args/input fence first, then collapse any
    -- expanded thinking tail.
    for _, el in ipairs(model.elements) do
      if el.type == 'tool_call' and el.args_open then
        el.args_open = false
        diff.updated_all[#diff.updated_all + 1] = el
      elseif el.type == 'subagent' and el.input_open then
        el.input_open = false
        diff.updated_all[#diff.updated_all + 1] = el
      end
    end
    merge_diff(diff, collapse_open_thinking(model))
    local el = add_element(model, {
      type = 'end_info',
      token_prefix = nil,
      tokens = {
        input_tokens = data.input_tokens,
        output_tokens = data.output_tokens,
        cache_creation_input_tokens = data.cache_creation_input_tokens,
        cache_read_input_tokens = data.cache_read_input_tokens,
      },
      end_status = data.end_status,
      error = data.error,
    })
    diff.added[#diff.added + 1] = el
    model.tail = el

  elseif variant == 'AssistantToolCallStart' then
    merge_diff(diff, flush_pending_whitespace(model))
    merge_diff(diff, collapse_open_thinking(model))
    local el = add_element(model, {
      type = 'tool_call',
      tool_call_id = data.tool_call_id,
      tool_name = data.tool_name or '',
      tool_call_index = data.tool_call_index or 0,
      created_at = data.created_at,
      args = '',
      args_open = true,
      output_started = false,
      output_open = false,
      output = '',
      status = 'generating',
      full_input = model.full_input,
      error = nil,
    })
    diff.added[#diff.added + 1] = el
    model.tail = el

  elseif variant == 'AssistantToolCallArgChunk' then
    -- Defensive collapse matches today's handler.
    merge_diff(diff, collapse_open_thinking(model))
    local el = find_tool_call_by_index(model, data.tool_call_index or 0)
    if el then
      local content = tostring(data.content)
      append_content(el, 'args', content)
      add_updated_content(diff, el, content)
    end
    -- missing mapping -> drop silently

  elseif variant == 'ToolMessageStart' then
    merge_diff(diff, collapse_open_thinking(model))
    local el = find_tool_call_by_id(model, data.tool_call_id)
    if el then
      -- Close the args fence, open output.
      el.args_open = false
      el.output_started = true
      el.status = 'running'
      el.output_open = true
      diff.updated_all[#diff.updated_all + 1] = el
      model.tail = el
    else
      -- Resumed-session fallback: no streamed args were seen. Render label +
      -- args fence (from tool_args when present) + open output fence.
      merge_diff(diff, flush_pending_whitespace(model))
      local new_el = add_element(model, {
        type = 'tool_call',
        tool_call_id = data.tool_call_id,
        tool_name = data.tool_name or '',
        tool_call_index = nil,
        created_at = data.created_at,
        args = '',
        args_open = false,
        output_started = true,
        output_open = true,
        output = '',
        status = 'running',
        full_input = model.full_input,
        error = nil,
      })
      if data.tool_args and data.tool_args ~= '' and data.tool_args ~= '{}' then
        new_el.args = data.tool_args
      end
      diff.added[#diff.added + 1] = new_el
      model.tail = new_el
    end

  elseif variant == 'ToolOutputChunk' then
    local el = find_tool_call_by_id(model, data.tool_call_id)
    if el then
      local content = tostring(data.content)
      append_content(el, 'output', content)
      add_updated_content(diff, el, content)
    else
      -- Fallback: today appends at the buffer tail, which in the model is the
      -- assistant message when it is the tail; otherwise drop.
      local tail = model.tail
      if tail and tail.type == 'assistant_message' then
        local content = tostring(data.content)
        append_content(tail, 'content', content)
        add_updated_content(diff, tail, content)
      end
    end

  elseif variant == 'ToolMessageEnd' then
    merge_diff(diff, collapse_open_thinking(model))
    local el = find_tool_call_by_id(model, data.tool_call_id)
    if el then
      el.output_open = false
      local status_map = {
        Succeeded = 'done', Failed = 'failed', Cancelled = 'cancelled',
        Timeout = 'failed', UserDenied = 'denied',
      }
      el.status = status_map[data.end_status] or 'done'
      diff.updated_all[#diff.updated_all + 1] = el
      local info = add_element(model, {
        type = 'end_info',
        token_prefix = 'TOOL',
        tokens = {
          input_tokens = data.input_tokens,
          output_tokens = data.output_tokens,
          cache_creation_input_tokens = nil,
          cache_read_input_tokens = nil,
        },
        end_status = data.end_status,
        error = data.error,
      })
      diff.added[#diff.added + 1] = info
      model.tail = info
    end
    -- element not found -> no-op

  elseif variant == 'ToolRequestPermission' then
    local el = find_tool_call_by_id(model, data.tool_call_id)
    if el then
      el.status = 'permission'
      diff.updated_all[#diff.updated_all + 1] = el
    end

  elseif variant == 'ToolPermissionApproved' then
    local el = find_tool_call_by_id(model, data.tool_call_id)
    if el then
      el.status = 'running'
      diff.updated_all[#diff.updated_all + 1] = el
    end

  elseif variant == 'SystemMessage' then
    merge_diff(diff, flush_pending_whitespace(model))
    merge_diff(diff, collapse_open_thinking(model))
    local el = add_element(model, {
      type = 'system_message',
      level = data.level or 'Info',
      message = data.message,
    })
    diff.added[#diff.added + 1] = el
    model.tail = el

  elseif variant == 'SubAgentInputStart' then
    merge_diff(diff, flush_pending_whitespace(model))
    merge_diff(diff, collapse_open_thinking(model))
    local el = add_element(model, {
      type = 'subagent',
      tool_call_id = data.tool_call_id,
      tool_call_index = data.tool_call_index or 0,
      conversation_id = nil,
      created_at = data.created_at,
      description = '',
      input = '',
      input_open = true,
      output = '',
      status = 'generating',
      is_continue = false,
      error = nil,
    })
    diff.added[#diff.added + 1] = el
    model.tail = el

  elseif variant == 'SubAgentInputChunk' then
    -- Defensive collapse matches today's handler.
    merge_diff(diff, collapse_open_thinking(model))
    local el = find_subagent_input_by_index(model, data.tool_call_index or 0)
    if el then
      local content = tostring(data.content)
      append_content(el, 'input', content)
      add_updated_content(diff, el, content)
    end
    -- missing -> drop silently

  elseif variant == 'SubAgentStart' then
    merge_diff(diff, collapse_open_thinking(model))
    local el = find_pending_subagent(model, data.tool_call_id)
    if el then
      el.input_open = false
      el.status = 'running'
      el.description = data.description or ''
      el.conversation_id = data.conversation_id
      diff.updated_all[#diff.updated_all + 1] = el
      model.tail = el
    else
      -- Resumed session: no pending input element was streamed.
      merge_diff(diff, flush_pending_whitespace(model))
      local new_el = add_element(model, {
        type = 'subagent',
        tool_call_id = data.tool_call_id,
        tool_call_index = nil,
        conversation_id = data.conversation_id,
        created_at = data.created_at,
        description = data.description or '',
        input = '',
        input_open = false,
        output = '',
        status = 'running',
        is_continue = false,
        error = nil,
      })
      diff.added[#diff.added + 1] = new_el
      model.tail = new_el
    end
    model.sa_active = data.conversation_id

  elseif variant == 'SubAgentContinue' then
    merge_diff(diff, collapse_open_thinking(model))
    local el = find_pending_subagent(model, data.tool_call_id)
    if el then
      -- The pending input element transforms in place into the continue
      -- section (one element per continue).
      local description = data.description
      if not description or description == '' then
        local last = find_subagent_by_conversation(model, data.conversation_id)
        description = last and last.description or ''
      end
      el.input_open = false
      el.status = 'continuing'
      el.is_continue = true
      el.description = description
      el.conversation_id = data.conversation_id
      diff.updated_all[#diff.updated_all + 1] = el
      model.tail = el
    else
      -- No pending input (resumed session): add a fresh continue element.
      merge_diff(diff, flush_pending_whitespace(model))
      local description = data.description
      if not description or description == '' then
        local last = find_subagent_by_conversation(model, data.conversation_id)
        description = last and last.description or ''
      end
      local new_el = add_element(model, {
        type = 'subagent',
        tool_call_id = data.tool_call_id,
        tool_call_index = nil,
        conversation_id = data.conversation_id,
        created_at = data.created_at,
        description = description,
        input = '',
        input_open = false,
        output = '',
        status = 'continuing',
        is_continue = true,
        error = nil,
      })
      diff.added[#diff.added + 1] = new_el
      model.tail = new_el
    end
    model.sa_active = data.conversation_id

  elseif variant == 'SubAgentTurnEnd' then
    local el = find_subagent_by_conversation(model, data.conversation_id)
    if el then
      el.status = (data.end_status and data.end_status ~= 'Succeeded') and data.end_status or 'turn ended'
      -- today's last-entry label shows [%d in / %d out]
      el.input_tokens = data.input_tokens
      el.output_tokens = data.output_tokens
      diff.updated_all[#diff.updated_all + 1] = el
    end
    if model.sa_active == data.conversation_id then
      model.sa_active = nil
    end

  elseif variant == 'SubAgentEnd' then
    merge_diff(diff, collapse_open_thinking(model))
    local status_text = (data.end_status and data.end_status ~= 'Succeeded') and data.end_status or 'done'
    for _, el in ipairs(model.elements) do
      if el.type == 'subagent' and el.conversation_id == data.conversation_id then
        el.status = status_text
        -- today's label renders [%d in / %d out] on every entry of the
        -- conversation, so the totals live on each element
        el.input_tokens = data.input_tokens
        el.output_tokens = data.output_tokens
        diff.updated_all[#diff.updated_all + 1] = el
      end
    end
    local last = find_subagent_by_conversation(model, data.conversation_id)
    if last then
      if type(data.error) == 'string' and data.error ~= '' then
        last.error = data.error
      end
    end
    if model.sa_active == data.conversation_id then
      model.sa_active = nil
    end

  elseif variant == 'SubAgentWaitingPermission' then
    local el = find_subagent_by_conversation(model, data.conversation_id)
    if el then
      el.status = 'permission'
      diff.updated_all[#diff.updated_all + 1] = el
    end

  elseif variant == 'SubAgentPermissionApproved' or variant == 'SubAgentPermissionDenied' then
    local el = find_subagent_by_conversation(model, data.conversation_id)
    if el then
      el.status = el.is_continue and 'continuing' or 'running'
      diff.updated_all[#diff.updated_all + 1] = el
    end

  elseif variant == 'AssistantMediaGenerating' then
    -- nothing to render in the model

  elseif variant == 'AssistantMediaOutput' then
    merge_diff(diff, flush_pending_whitespace(model))
    merge_diff(diff, collapse_open_thinking(model))
    if data.media and data.media.relative_path then
      local el = add_element(model, {
        type = 'media',
        relative_path = data.media.relative_path,
      })
      diff.added[#diff.added + 1] = el
      model.tail = el
    end

  elseif variant == 'LLMRetry' then
    merge_diff(diff, flush_pending_whitespace(model))
    merge_diff(diff, collapse_open_thinking(model))
    local el = add_element(model, {
      type = 'retry',
      attempt = data.attempt or 1,
      max_retries = data.max_retries or 0,
      reason = data.reason or '',
    })
    diff.added[#diff.added + 1] = el
    model.tail = el

  elseif variant == 'AssistantRequestEnd' then
    merge_diff(diff, flush_pending_whitespace(model))
    merge_diff(diff, collapse_open_thinking(model))
    local el = add_element(model, {
      type = 'end_marker',
      tokens = {
        total_input_tokens = data.total_input_tokens,
        total_cache_creation_tokens = data.total_cache_creation_tokens,
        total_cache_read_tokens = data.total_cache_read_tokens,
        total_output_tokens = data.total_output_tokens,
      },
    })
    diff.added[#diff.added + 1] = el
    model.tail = el

  elseif variant == 'UserRequestEnd' or variant == 'PermissionUpdated' then
    -- no-op
  end

  return diff
end

-- ============================================================================
-- RENDERER
-- ============================================================================
-- The ONLY layer that touches the buffer / extmark / highlight APIs. Consumes
-- the reducer's diff contract: { added = {el,...}, updated_all = {el,...},
-- updated_content = {{el, text},...} } and projects the model onto the buffer.
-- render(model, diff, ctx) / render_batch(model, diffs, ctx) are the entry
-- points; ctx = { buf, ns, bulk, width, media_root }. Element
-- positions are plain integers in the row map, maintained by three
-- incremental operations (append / stream / rebuild) — never extmark
-- anchors, never a full-buffer rebuild in the live path. Extmarks are
-- decoration only (per-row / per-col-range highlights).

-- Renderer-owned bookkeeping keyed per model (weak keys: a discarded model
-- releases its state). rows = el.id -> { start_row = <int or nil>, height =
-- <int> }, the integer row map maintained by the render operations (a
-- zero-height element — media without a root, empty end_info — has no
-- start_row and is skipped by row lookup and shift arithmetic); hl =
-- el.id -> list of { ns, id } highlight extmarks the element owns, deleted
-- before a rebuild re-applies them; first_event = true until the first
-- projection replaces the buffer's initial single empty line.
local renderer_state = setmetatable({}, { __mode = 'k' })

local function get_renderer_state(model)
  local st = renderer_state[model]
  if not st then
    -- rows: element id -> { start_row, height }; attach: am id -> ordered
    -- list of attached block ids; host: block id -> owning am id.
    st = { rows = {}, hl = {}, attach = {}, host = {}, first_event = true }
    renderer_state[model] = st
  end
  return st
end

-- Split text on '\n' into buffer rows. lines('') = { '' } (one empty row):
-- the streaming blank that content chunks consume. A NUL byte is an internal
-- line break to nvim's buffer API, so each returned line also escapes every
-- \0 as the two-byte display form '\0' — a projected row must stay one
-- buffer row or the row map desyncs. Display-only: the model keeps the raw
-- bytes.
local function lines(text)
  local out = vim.split(text or '', '\n', { plain = true })
  for i = 1, #out do
    out[i] = escape_nul(out[i])
  end
  return out
end

-- Collapse embedded newlines in wire-derived text: a '\n' in a status / name /
-- description must stay on one buffer row (nil-safe). NUL bytes are escaped
-- the same way as lines() so single_line'd chrome text can never inject an
-- internal buffer line break.
local function single_line(s)
  return escape_nul(tostring(s or ''):gsub('\n', ' '))
end

-- Place a full-row highlight extmark for a rebuilt element kind and track its
-- id in state.hl so a later rebuild can delete it. nvim_buf_add_highlight
-- returns no usable id on this build (its return value is not the created
-- extmark id), so the region element kinds place highlights with
-- nvim_buf_set_extmark instead. The mark shape (end_row = row + 1, end_col = 0)
-- matches nvim_buf_add_highlight's full-row highlight exactly; this build
-- rejects end_col = -1.
local function add_tracked_highlight(state, el, buf, ns, group, row)
  local id = vim.api.nvim_buf_set_extmark(buf, ns, row, 0, {
    hl_group = group, end_row = row + 1, end_col = 0,
  })
  local list = state.hl[el.id]
  if not list then list = {}; state.hl[el.id] = list end
  list[#list + 1] = { ns, id }
  return id
end

-- Place a COL-RANGE highlight extmark (decoration only) for a chrome line
-- part and track its id in state.hl. Priority 150 beats the treesitter
-- @comment highlight (default priority 100) that the tcode grammar applies
-- to '► ' separator lines, so the per-part chrome colors win. Columns are
-- byte offsets; end_col is exclusive. The row text and the ranges come from
-- the same parts builder, so they can never disagree.
local function add_tracked_col_highlight(state, el, buf, ns, group, row, start_col, end_col)
  local id = vim.api.nvim_buf_set_extmark(buf, ns, row, start_col, {
    hl_group = group, end_row = row, end_col = end_col, priority = 150,
  })
  local list = state.hl[el.id]
  if not list then list = {}; state.hl[el.id] = list end
  list[#list + 1] = { ns, id }
  return id
end

-- Delete every highlight extmark an element owns (pcall'd: a mark may already
-- be gone if the buffer was replaced wholesale) and clear the tracking list.
-- Called BEFORE the rebuild's set_lines: on this build replaced-region marks
-- are not deleted, they slide to the row past the region end, so leaving them
-- would accumulate an unbounded stack of stale highlights across rebuilds.
local function del_hl_marks(state, buf, el_id)
  local list = state.hl[el_id]
  if list then
    for _, entry in ipairs(list) do
      pcall(vim.api.nvim_buf_del_extmark, buf, entry[1], entry[2])
    end
    state.hl[el_id] = nil
  end
end

-- Tool-call label status (status text + cancel hint) rendered by
-- project_element.
local TC_STATUS = {
  generating = { text = 'generating', hl = 'TCodeTool', cancel = true },
  running = { text = 'running', hl = 'TCodeTool', cancel = true },
  permission = { text = 'permission', hl = 'TCodePermission' },
  done = { text = 'done', hl = 'TCodeSuccess' },
  failed = { text = 'failed', hl = 'TCodeError' },
  cancelled = { text = 'cancelled', hl = 'TCodeError' },
  denied = { text = 'denied', hl = 'TCodeError' },
}

local function system_message_hl(level)
  if level == 'Warning' then return 'TCodeSystemWarning' end
  if level == 'Error' then return 'TCodeSystemError' end
  return 'TCodeSystemInfo'
end

-- Tail display: the last at most n REAL lines of a content blob. A single
-- trailing empty row left by a trailing '\n' is trimmed so 'a\n' counts as
-- one line; empty content yields {} (callers render the one-empty-line
-- placeholder where a section requires one).
local function tail_lines(content, n)
  if content == '' then return {} end
  local ls = lines(content)
  if #ls > 1 and ls[#ls] == '' then table.remove(ls) end
  if #ls <= n then return ls end
  local tail = {}
  for i = #ls - n + 1, #ls do tail[#tail + 1] = ls[i] end
  return tail
end

-- Tail display with a DISPLAY-SPACE cap: the last at most n real lines AND at
-- most width*n bytes, so a section never renders more than n visual rows of a
-- full-width window regardless of line length (a single very long line is cut
-- to its last width*n bytes instead of wrapping indefinitely). Tail-biased:
-- the newest content wins — slice to the LAST width*n bytes, then take the
-- last n lines of that slice. Byte-based, consistent with the other width
-- approximations (Lua # and string sub are byte-based); a slice that starts
-- mid-line is fine for a tail view. A byte slice CAN cut a multi-byte UTF-8
-- character in half, so after slicing any leading UTF-8 continuation bytes
-- (0x80-0xBF, at most 3) are dropped: the slice then starts at a complete
-- character. The slice END is the content's real end, so only the start needs
-- fixing (pure best-effort: valid UTF-8 always yields a clean slice).
local function tail_capped(content, width, n)
  if content == '' then return {} end
  local char_cap = math.max(1, (width or 80) * n)
  if #content > char_cap then
    content = content:sub(-char_cap)
    local i = 1
    while i <= 3 do
      local b = content:byte(i)
      if b and b >= 0x80 and b <= 0xBF then
        i = i + 1
      else
        break
      end
    end
    if i > 1 then content = content:sub(i) end
  end
  return tail_lines(content, n)
end

-- Subagent label status: status text + highlight group (the old pre-rewrite
-- label colors). Wire-derived end_status strings (anything not in the map)
-- fall through to TCodeError.
local function subagent_status_parts(status)
  local map = {
    generating = { 'generating', 'TCodeTool' },
    running = { 'running', 'TCodeTool' },
    continuing = { 'continuing', 'TCodeTool' },
    permission = { 'permission', 'TCodePermission' },
    ['turn ended'] = { 'turn ended', 'TCodeTokens' },
    done = { 'done', 'TCodeSuccess' },
  }
  local key = status or 'done'
  local entry = map[key]
  if entry then return entry[1], entry[2] end
  return single_line(key), 'TCodeError'
end

-- Concatenate one chrome row's { text, group } parts into
-- { text, spans } where spans is the list of { start_col, end_col, group }
-- byte-column ranges (end_col exclusive, non-overlapping — the parts tile
-- the line contiguously). Columns are bytes ('►' is 3 bytes; #s is bytes).
local function chrome_row(parts)
  local text = {}
  local spans = {}
  local col = 0
  for _, part in ipairs(parts) do
    text[#text + 1] = part[1]
    spans[#spans + 1] = { col, col + #part[1], part[2] }
    col = col + #part[1]
  end
  return { text = table.concat(text), spans = spans }
end

-- Project one element to its FULL layout: a list of { text, spans } rows in
-- buffer order. spans (nil for content rows) carries the col-range chrome
-- highlight parts. This is the single source of truth for project_element
-- (which concatenates the texts) and element_chrome_spans (which maps the
-- ranges to element-relative rows), so the chrome text and its colors can
-- never drift. Every chrome line starts with '► '; content lines (fences,
-- message/tool/subagent data) carry no prefix. Fences stay content and the
-- new section headers sit outside the fence pairs. ctx = { width,
-- media_root } carries only read-only rendering inputs: the media URI
-- root. No buffer / extmark / navigation state.
local function element_layout(el, ctx)
  local function c_row(text)
    return { text = text }
  end
  local width = (ctx and ctx.width) or 80
  if el.type == 'user_message' then
    local ts = format_time(el.created_at)
    local parts = { { '► USER', 'TCodeUser' } }
    if ts then parts[#parts + 1] = { '  ' .. ts, 'TCodeTokens' } end
    local out = { chrome_row(parts) }
    local content = content_of(el, 'content')
    if content ~= '' then
      for _, l in ipairs(lines(content)) do out[#out + 1] = c_row(l) end
    end
    return out
  elseif el.type == 'assistant_message' then
    local ts = format_time(el.created_at)
    local parts = { { '► ASSISTANT', 'TCodeAssistant' } }
    if ts then parts[#parts + 1] = { '  ' .. ts, 'TCodeTokens' } end
    local out = { chrome_row(parts) }
    local content = content_of(el, 'content')
    if content ~= '' then
      for _, l in ipairs(lines(content)) do out[#out + 1] = c_row(l) end
    end
    return out
  elseif el.type == 'thinking_block' then
    if el.state == 'collapsed' then
      return { chrome_row({ { '► [Thinking... press o to expand]', 'TCodeTokens' } }) }
    end
    -- expanded: chrome row + full content.
    local out = { chrome_row({ { '► [Thinking... press o to collapse]', 'TCodeTokens' } }) }
    for _, l in ipairs(lines(content_of(el, 'content'))) do out[#out + 1] = c_row(l) end
    return out
  elseif el.type == 'tool_call' then
    local s = TC_STATUS[el.status] or { text = 'done', hl = 'TCodeSuccess' }
    local parts = { { '► TOOL:', 'TCodeTool' }, { ' [' .. s.text .. ']', s.hl } }
    if el.tool_name and el.tool_name ~= '' then
      parts[#parts + 1] = { ' ' .. single_line(el.tool_name), 'TCodeTool' }
    end
    local ts = format_time(el.created_at)
    if ts then parts[#parts + 1] = { '  ' .. ts, 'TCodeTokens' } end
    if s.cancel then parts[#parts + 1] = { '  [Ctrl-k to cancel]', 'TCodeTokens' } end
    local out = { chrome_row(parts) }
    local function section_lines(text)
      -- Display window: a 5-line / width*5-byte tail window. Detail view
      -- (full_input): the complete content, never truncated.
      if el.full_input then return lines(text) end
      return tail_capped(text, width, 5)
    end
    local args = content_of(el, 'args')
    if args ~= '' then
      out[#out + 1] = chrome_row({ { '► Param', 'TCodeTokens' } })
      out[#out + 1] = c_row(TC_FENCE)
      for _, l in ipairs(section_lines(args)) do out[#out + 1] = c_row(l) end
      out[#out + 1] = c_row(TC_FENCE)
    end
    if el.output_started then
      out[#out + 1] = chrome_row({ { '► Result', 'TCodeTokens' } })
      out[#out + 1] = c_row(TC_FENCE)
      local out_lines = section_lines(content_of(el, 'output'))
      if #out_lines == 0 then
        out[#out + 1] = c_row('')
      else
        for _, l in ipairs(out_lines) do out[#out + 1] = c_row(l) end
      end
      out[#out + 1] = c_row(TC_FENCE)
    end
    return out
  elseif el.type == 'subagent' then
    local status_text, status_hl = subagent_status_parts(el.status)
    local parts = { { '► SUB-AGENT:', 'TCodeTool' }, { ' [' .. status_text .. ']', status_hl } }
    local ts = format_time(el.created_at)
    if ts then parts[#parts + 1] = { '  ' .. ts, 'TCodeTokens' } end
    if el.input_tokens and el.output_tokens then
      parts[#parts + 1] = { string.format('  [%d in / %d out]', el.input_tokens, el.output_tokens), 'TCodeTokens' }
    end
    local desc = single_line(el.description)
    if desc ~= '' then parts[#parts + 1] = { '  ' .. desc, 'TCodeTool' } end
    local out = { chrome_row(parts) }
    local function section_lines(text)
      -- Display window: a 5-line / width*5-byte tail window. Detail view
      -- (full_input): the complete content, never truncated.
      if el.full_input then return lines(text) end
      return tail_capped(text, width, 5)
    end
    local input = content_of(el, 'input')
    if input ~= '' then
      out[#out + 1] = chrome_row({ { '► Input', 'TCodeTokens' } })
      out[#out + 1] = c_row(TC_FENCE)
      for _, l in ipairs(section_lines(input)) do out[#out + 1] = c_row(l) end
      out[#out + 1] = c_row(TC_FENCE)
    end
    if not el.input_open then
      out[#out + 1] = chrome_row({ { '► Output', 'TCodeTokens' } })
      out[#out + 1] = c_row(TC_FENCE)
      local out_lines = section_lines(content_of(el, 'output'))
      if #out_lines == 0 then
        out[#out + 1] = c_row('')
      else
        for _, l in ipairs(out_lines) do out[#out + 1] = c_row(l) end
      end
      if el.error then
        out[#out + 1] = c_row('')
        for _, l in ipairs(lines('Error: ' .. el.error)) do out[#out + 1] = c_row(l) end
      end
      out[#out + 1] = c_row(TC_FENCE)
    end
    return out
  elseif el.type == 'system_message' then
    local level = single_line(el.level or 'Info')
    local out = { chrome_row({ { '► SYSTEM [' .. level .. ']', system_message_hl(el.level) } }) }
    for _, l in ipairs(lines(el.message)) do out[#out + 1] = c_row(l) end
    return out
  elseif el.type == 'media' then
    local media_root = ctx and ctx.media_root
    if not media_root or not el.relative_path or el.relative_path == '' then
      return {}
    end
    return { c_row(''), c_row(escape_nul('![img](file://' .. media_root .. el.relative_path .. ')')) }
  elseif el.type == 'retry' then
    -- The reason may be a multi-line message (e.g. a JSON error body); split
    -- it so no buffer row carries an embedded newline.
    local reason_lines = lines(el.reason or '')
    local out = {
      chrome_row({ { '► [' .. string.format('Retrying... (attempt %d/%d) -- %s]', el.attempt or 1, el.max_retries or 0, reason_lines[1]), 'TCodeTokens' } }),
    }
    for i = 2, #reason_lines do out[#out + 1] = c_row(reason_lines[i]) end
    return out
  elseif el.type == 'end_info' then
    local tokens = el.tokens or {}
    local token_prefix = el.token_prefix
    local token_line
    if tokens.input_tokens and tokens.output_tokens then
      local has_tokens = not token_prefix or (tokens.input_tokens > 0 or tokens.output_tokens > 0)
      if has_tokens then
        local cache_read = tokens.cache_read_input_tokens or 0
        local processed = tokens.input_tokens + (tokens.cache_creation_input_tokens or 0)
        local seg = cache_read > 0
          and (processed .. ' in / ' .. cache_read .. ' cache read / ' .. tokens.output_tokens .. ' out tokens')
          or (processed .. ' in / ' .. tokens.output_tokens .. ' out tokens')
        token_line = '[' .. (token_prefix and (token_prefix .. ': ') or '') .. seg .. ']'
      end
    end
    local status_text = (el.end_status and el.end_status ~= 'Succeeded')
      and single_line(el.end_status) or nil
    local out = {}
    if token_line and status_text then
      out[#out + 1] = chrome_row({
        { '► ' .. token_line, 'TCodeTokens' },
        { ' [' .. status_text .. ']', 'TCodeError' },
      })
    elseif token_line then
      out[#out + 1] = chrome_row({ { '► ' .. token_line, 'TCodeTokens' } })
    elseif status_text then
      out[#out + 1] = chrome_row({ { '► [' .. status_text .. ']', 'TCodeError' } })
    end
    if type(el.error) == 'string' and el.error ~= '' then
      for _, l in ipairs(lines('Error: ' .. el.error)) do out[#out + 1] = c_row(l) end
    end
    return out
  elseif el.type == 'end_marker' then
    local tokens = el.tokens or {}
    local total_cache_read = tokens.total_cache_read_tokens or 0
    local total_processed = (tokens.total_input_tokens or 0) + (tokens.total_cache_creation_tokens or 0)
    local total_output = tokens.total_output_tokens or 0
    if total_cache_read > 0 then
      return {
        chrome_row({ { '► ' .. string.format('[Total: %d in / %d cache read / %d out tokens]', total_processed, total_cache_read, total_output), 'TCodeTokens' } }),
      }
    end
    return { chrome_row({ { '► ' .. string.format('[Total: %d in / %d out tokens]', total_processed, total_output), 'TCodeTokens' } }) }
  end
  return {}
end

-- Project one element to its real buffer lines (possibly empty). Pure with
-- respect to element state + ctx: no buffer / extmark / navigation state.
-- Derives the text from the shared element_layout so the chrome text always
-- matches the col-range highlights of element_chrome_spans.
local function project_element(el, ctx)
  local layout = element_layout(el, ctx)
  local out = {}
  for _, row in ipairs(layout) do out[#out + 1] = row.text end
  return out
end

-- Pure chrome-span lookup: element-relative { row, start_col, end_col, group }
-- byte-column highlight ranges for the element's chrome lines, derived from
-- the SAME layout builder as project_element so text and colors cannot
-- drift. Columns are bytes ('►' is 3 bytes); spans are non-overlapping per
-- row. Content rows contribute no spans — they get the full-row content
-- highlights in apply_element_highlights.
local function element_chrome_spans(el, ctx)
  local layout = element_layout(el, ctx)
  local spans = {}
  for r, row in ipairs(layout) do
    if row.spans then
      for _, sp in ipairs(row.spans) do
        spans[#spans + 1] = { row = r - 1, start_col = sp[1], end_col = sp[2], group = sp[3] }
      end
    end
  end
  return spans
end

-- Pure navigation intent: what `o` does on a row of the layout.
-- 'thinking' toggles a thinking block from ANY row, in either state;
-- 'detail' opens the tool-call / subagent detail view from EVERY row of a
-- tool_call or subagent element (label, section headers, and content rows
-- — the expand/collapse sections are gone, so nothing else toggles); nil
-- means nothing under the cursor. The tool_call_id / conversation_id
-- guards on 'detail' belong to the keymap caller, not this lookup.
local function action_at(el)
  if not el then return nil end
  if el.type == 'thinking_block' then
    return 'thinking'
  elseif el.type == 'tool_call' or el.type == 'subagent' then
    return 'detail'
  end
  return nil
end

-- Row-map entry for an element: { start_row = <int or nil>, height = <int> }.
-- Zero-height elements carry no start_row so row lookup skips them.
local function element_row(state, el)
  return state.rows[el.id]
end

local function set_element_row(state, el, start_row, height)
  state.rows[el.id] = { start_row = start_row, height = height }
end

-- Fill in the read-only rendering inputs a caller may omit: the display width
-- (live window width, default 80 — kept for API stability; the tail-cap
-- projection no longer truncates) and the media root precomputed once per
-- batch from M.display_file (the uri-encoded absolute session media dir).
local function fill_ctx(ctx)
  if not ctx.width then
    local win = vim.fn.bufwinid(ctx.buf)
    ctx.width = (win ~= -1) and vim.api.nvim_win_get_width(win) or 80
  end
  if ctx.media_root == nil and M.display_file then
    local session_dir = vim.fn.fnamemodify(M.display_file, ':h')
    ctx.media_root = vim.uri_encode(session_dir .. '/media/')
  end
end

-- Per-row highlight decoration for an element's projected lines. Chrome rows
-- get col-range marks from element_chrome_spans (priority 150, beating the
-- treesitter @comment default of 100); content rows keep the full-row logic:
-- the element's group (subagent error rows get TCodeError; the whole retry
-- block is TCodeTokens). Every mark is tracked in state.hl so a rebuild can
-- delete and re-apply them. Extmarks are decoration only — they never carry
-- position, text, or navigation data.
local function apply_element_highlights(state, buf, ns, el, ctx, start_row, el_lines)
  -- Structural chrome-row set: element-relative indices of the layout's
  -- chrome rows (from the SAME layout builder that produced el_lines), so
  -- content classification never matches the content text — a content row
  -- that itself starts with '► ' is still content.
  local chrome = {}
  for _, sp in ipairs(element_chrome_spans(el, ctx)) do
    chrome[sp.row] = true
    add_tracked_col_highlight(state, el, buf, ctx.ns, sp.group, start_row + sp.row, sp.start_col, sp.end_col)
  end
  local function is_content(i)
    -- Every non-chrome row except fence rows is content (i is 1-based; the
    -- chrome set is keyed by 0-based element-relative row).
    return not chrome[i - 1] and el_lines[i] ~= TC_FENCE
  end
  local group
  if el.type == 'thinking_block' then
    group = 'TCodeThinking'
  elseif el.type == 'tool_call' then
    group = 'TCodeToolArgs'
  elseif el.type == 'system_message' then
    group = system_message_hl(el.level)
  elseif el.type == 'end_info' then
    group = 'TCodeError'
  end
  for i = 1, #el_lines do
    local line = el_lines[i]
    local g
    if el.type == 'subagent' then
      if line:match('^Error: ') then
        g = 'TCodeError'
      elseif is_content(i) then
        g = 'TCodeToolArgs'
      end
    elseif el.type == 'retry' then
      g = 'TCodeTokens'
    elseif group and is_content(i) then
      g = group
    end
    if g then
      add_tracked_highlight(state, el, buf, ns, g, start_row + i - 1)
    end
  end
end

-- Shift every element AFTER el in the model by delta rows: only start_row
-- moves (the buffer rows themselves were already moved by the write).
-- Zero-height elements have no start_row and are skipped. Elements ATTACHED
-- to el (state.host[e.id] == el.id) are skipped too: an am's attached blocks
-- live INSIDE the am's region, so when the am's own rows grow below them
-- (e.g. streamed content) they must not move; they move only via their own
-- rebuilds (which shift later elements, including sibling attached blocks).
local function shift_later_elements(model, state, el, delta)
  if delta == 0 then return end
  local past = false
  for _, e in ipairs(model.elements) do
    if past then
      if state.host[e.id] ~= el.id then
        local entry = state.rows[e.id]
        if entry and entry.start_row then
          entry.start_row = entry.start_row + delta
        end
      end
    elseif e == el then
      past = true
    end
  end
end

-- Operation 1 (append): render a newly added element. The first element
-- replaces the buffer's initial single empty line (start_row = 0); later
-- elements append at the buffer tail. Zero-height projections (media without
-- a root, an empty end_info) are recorded with height 0 and no start_row.
-- A thinking block tagged attach_to (created while the model tail was an
-- assistant_message) is inserted INSIDE its host am's region at the am's
-- current region end — the block's arrival position — so the display order
-- matches the wire order (thinking before response). The am's region then
-- spans [label] + attached blocks + content, contiguous.
local function render_added(model, el, state, ctx)
  local buf = ctx.buf
  local el_lines = project_element(el, ctx)
  if not el_lines or #el_lines == 0 then
    -- Zero-height (media without a root, empty end_info): recorded but
    -- never written. first_event must SURVIVE here: this branch does not
    -- touch the buffer, so the initial single empty line is still waiting
    -- to be replaced by the first real projection.
    set_element_row(state, el, nil, 0)
    return
  end
  if el.attach_to then
    local am = model.by_id[el.attach_to]
    local am_entry = am and state.rows[am.id]
    if am_entry and am_entry.start_row ~= nil then
      local ins = am_entry.start_row + am_entry.height
      vim.api.nvim_buf_set_lines(buf, ins, ins, false, el_lines)
      set_element_row(state, el, ins, #el_lines)
      state.host[el.id] = el.attach_to
      local list = state.attach[el.attach_to]
      if not list then list = {}; state.attach[el.attach_to] = list end
      list[#list + 1] = el.id
      am_entry.height = am_entry.height + #el_lines
      shift_later_elements(model, state, am, #el_lines)
      apply_element_highlights(state, buf, ctx.ns, el, ctx, ins, el_lines)
      return
    end
    -- Defensive fall-through: the am has no row entry (never rendered or
    -- zero-height); treat this block as a normal append below.
  end
  local start_row
  if state.first_event and vim.api.nvim_buf_line_count(buf) == 1 then
    state.first_event = false
    vim.api.nvim_buf_set_lines(buf, 0, 1, false, el_lines)
    start_row = 0
  else
    state.first_event = false
    start_row = vim.api.nvim_buf_line_count(buf)
    append_lines(buf, el_lines)
  end
  set_element_row(state, el, start_row, #el_lines)
  apply_element_highlights(state, buf, ctx.ns, el, ctx, start_row, el_lines)
end

-- Forward declaration: operation 3 (render_rebuild) is defined below but the
-- stream operation delegates to it for tool / subagent elements, so the
-- local must exist at the call site.
local render_rebuild

-- Operation 2 (stream): append a content delta onto element E's OWN rows
-- (never "the buffer tail" — the bug-2 fix). tool_call / subagent elements
-- delegate to render_rebuild instead: their 5-line tail cap bounds the whole
-- element to O(cap) rows, so a bounded in-place rebuild per streaming chunk
-- keeps the fences / headers / row map exact and never touches other
-- elements or the whole buffer. Assistant / thinking keep the append path:
-- the chrome/content split is STRUCTURAL — E "has content rows" iff its
-- height exceeds the chrome rows (1 for an assistant_message and a thinking
-- block) plus the heights of its attached blocks. With no content
-- rows yet the delta inserts as new rows at the region end (after the am
-- label, or below the last attached block — the wire order
-- thinking-then-response maps to block rows then content rows); with content
-- rows the delta appends onto E's last content row at E.start_row +
-- E.height - 1: the delta's first line joins it and subsequent lines insert
-- below (a leading newline in the delta joins nothing, so the result always
-- matches a full projection of the appended content). The decision never
-- matches the content text, so content that itself starts with '► ' joins
-- correctly. Then added_rows = count_lines(delta) - 1, E.height grows by it,
-- every later element's start_row shifts by it, and the new rows get
-- highlighted. An attached thinking block's growth also grows its host am's
-- region.
local function render_updated_content(model, entry, state, ctx)
  local buf = ctx.buf
  local el = entry[1]
  local text = updated_content_text(entry)
  if text == '' then return end
  if el.type == 'tool_call' or el.type == 'subagent' then
    render_rebuild(model, el, state, ctx)
    return
  end
  local row_entry = state.rows[el.id]
  if not row_entry or row_entry.start_row == nil then return end
  local target_row = row_entry.start_row + row_entry.height - 1
  local join_col = #(vim.api.nvim_buf_get_lines(buf, target_row, target_row + 1, false)[1] or '')
  local added_rows
  -- Structural chrome/content decision: the element "has content rows" iff
  -- its region height exceeds the chrome rows (1 for an assistant_message
  -- and a thinking block) plus the heights of its attached blocks. The am's
  -- trailing case stays structural too: when the region ENDS with an
  -- attached block (or holds only the label), the delta starts a fresh
  -- segment below it — the wire order thinking-then-response — it never
  -- joins onto the block. Never decided by matching the content text, so
  -- content that itself starts with '► ' still joins correctly.
  local chrome_rows = 1
  local attached_sum = 0
  local trailing_block = false
  if el.type == 'assistant_message' then
    local region_end = row_entry.start_row + row_entry.height
    local attached = state.attach[el.id]
    if attached then
      for _, bid in ipairs(attached) do
        local be = state.rows[bid]
        if be and be.height then
          attached_sum = attached_sum + be.height
          if bid == attached[#attached] and be.start_row
            and be.start_row + be.height == region_end then
            trailing_block = true
          end
        end
      end
    end
  end
  if el.type == 'assistant_message' and (trailing_block or row_entry.height <= chrome_rows + attached_sum) then
    -- No content rows below the region end (the region ends with an
    -- attached block, or holds only the label): insert the delta as new
    -- rows at the region end — the wire order thinking-then-response.
    local region_end = row_entry.start_row + row_entry.height
    vim.api.nvim_buf_set_lines(buf, region_end, region_end, false, lines(text))
    added_rows = count_lines(text)
  else
    -- Content rows exist (non-am elements always have them: a zero-height
    -- element was skipped above): join the delta onto the last content row
    -- (its first line joins, subsequent lines insert below).
    insert_text_at(buf, target_row, text)
    added_rows = count_lines(text) - 1
  end
  row_entry.height = row_entry.height + added_rows
  shift_later_elements(model, state, el, added_rows)
  if el.type == 'thinking_block' and state.host[el.id] then
    -- An attached block's growth grows its host am's region with it.
    local host_entry = state.rows[state.host[el.id]]
    if host_entry then host_entry.height = host_entry.height + added_rows end
  end
  if added_rows > 0 and el.type == 'thinking_block' then
    -- A multi-line delta inserted into a ZERO-LENGTH join row slides the
    -- join row's own right-gravity highlight mark onto the inserted block;
    -- delete every slid mark and re-place the join row's so each row keeps
    -- exactly one.
    if join_col == 0 then
      local list = state.hl[el.id]
      if list then
        for j = #list, 1, -1 do
          local entry = list[j]
          local pos = vim.api.nvim_buf_get_extmark_by_id(buf, entry[1], entry[2], {})
          if pos and pos[1] > target_row then
            pcall(vim.api.nvim_buf_del_extmark, buf, entry[1], entry[2])
            table.remove(list, j)
          end
        end
      end
      add_tracked_highlight(state, el, buf, ctx.ns, 'TCodeThinking', target_row)
    end
    for i = target_row + 1, target_row + added_rows do
      add_tracked_highlight(state, el, buf, ctx.ns, 'TCodeThinking', i)
    end
  end
end

-- Fold an assistant_message's attached blocks into its projection at their
-- arrival position: [label] + <each attached block's lines, in model order>
-- + <the am's content lines>. Returns (lines, block_entries) where
-- block_entries[b_id] = { start_row, height } gives each block's sub-entry
-- RELATIVE to the am's region start. Blocks are located by attach_to (they
-- are the elements whose attach_to == am.id; the am always precedes them).
-- The model stores the am's content as one field, so an interleaved
-- content/block split cannot be reconstructed — blocks group before content
-- (matches the common case exactly: thinking runs precede the response).
local function fold_am_blocks(model, ctx, am)
  local layout = element_layout(am, ctx)
  local out = { layout[1].text }
  local block_entries = {}
  local row = 1
  for _, e in ipairs(model.elements) do
    if e.attach_to == am.id then
      local blk_lines = project_element(e, ctx) or {}
      block_entries[e.id] = { start_row = row, height = #blk_lines }
      for _, l in ipairs(blk_lines) do out[#out + 1] = l end
      row = row + #blk_lines
    end
  end
  for i = 2, #layout do out[#out + 1] = layout[i].text end
  return out, block_entries
end

-- Rebuild an assistant_message that carries attached blocks: fold the
-- blocks back into the am's region (label + blocks + content, contiguous)
-- and re-place every sub-entry. Used ONLY as a defensive guard — the
-- reducer never tags an am as updated_all (ams stream via updated_content),
-- so the normal path cannot corrupt an attached-block region.
local function render_am_rebuild(model, el, state, ctx)
  local buf = ctx.buf
  local row_entry = state.rows[el.id]
  if not row_entry or row_entry.start_row == nil then return end
  local start_row = row_entry.start_row
  local old_height = row_entry.height or 0
  del_hl_marks(state, buf, el.id)
  for _, bid in ipairs(state.attach[el.id]) do
    del_hl_marks(state, buf, bid)
  end
  local all_lines, block_entries = fold_am_blocks(model, ctx, el)
  if #all_lines == 0 then
    -- Defensive: nothing to project; remove the whole region.
    vim.api.nvim_buf_set_lines(buf, start_row, start_row + old_height, false, {})
    set_element_row(state, el, nil, 0)
    for bid in pairs(block_entries) do
      set_element_row(state, model.by_id[bid], nil, 0)
    end
    shift_later_elements(model, state, el, -old_height)
    return
  end
  vim.api.nvim_buf_set_lines(buf, start_row, start_row + old_height, false, all_lines)
  set_element_row(state, el, start_row, #all_lines)
  for bid, info in pairs(block_entries) do
    set_element_row(state, model.by_id[bid], start_row + info.start_row, info.height)
  end
  shift_later_elements(model, state, el, #all_lines - old_height)
  -- Re-apply the am's chrome (the label) and each block's full highlights.
  for _, sp in ipairs(element_chrome_spans(el, ctx)) do
    add_tracked_col_highlight(state, el, buf, ctx.ns, sp.group, start_row + sp.row, sp.start_col, sp.end_col)
  end
  for bid, info in pairs(block_entries) do
    apply_element_highlights(state, buf, ctx.ns, model.by_id[bid], ctx,
      start_row + info.start_row, project_element(model.by_id[bid], ctx))
  end
end

-- Operation 3 (rebuild): replace element E's region in place from full model
-- state (updated_all: toggle / collapse / expand / status change / fence
-- close). Delete E's tracked highlight extmarks, project fresh
-- lines, replace E's region, shift later elements by the height delta, set
-- E's new height, and re-apply the highlights. An attached block's rebuild
-- also grows/shrinks its host am's region with it.
render_rebuild = function(model, el, state, ctx)
  local buf = ctx.buf
  local row_entry = state.rows[el.id]
  if not row_entry or row_entry.start_row == nil then return end
  -- Defensive guard: an am with attached blocks is rebuilt by folding the
  -- blocks back into the region (never silently dropping them).
  if el.type == 'assistant_message' and state.attach[el.id] and #state.attach[el.id] > 0 then
    render_am_rebuild(model, el, state, ctx)
    return
  end
  local start_row = row_entry.start_row
  local old_height = row_entry.height or 0
  del_hl_marks(state, buf, el.id)
  local new_lines = project_element(el, ctx) or {}
  if #new_lines == 0 then
    -- Defensive: the element now projects no rows; remove its region.
    vim.api.nvim_buf_set_lines(buf, start_row, start_row + old_height, false, {})
    set_element_row(state, el, nil, 0)
    shift_later_elements(model, state, el, -old_height)
    if state.host[el.id] then
      local host_entry = state.rows[state.host[el.id]]
      if host_entry then host_entry.height = host_entry.height - old_height end
    end
    return
  end
  vim.api.nvim_buf_set_lines(buf, start_row, start_row + old_height, false, new_lines)
  local delta = #new_lines - old_height
  set_element_row(state, el, start_row, #new_lines)
  shift_later_elements(model, state, el, delta)
  if state.host[el.id] then
    -- An attached block's rebuild grows/shrinks its host am's region with it.
    local host_entry = state.rows[state.host[el.id]]
    if host_entry then host_entry.height = host_entry.height + delta end
  end
  apply_element_highlights(state, buf, ctx.ns, el, ctx, start_row, new_lines)
end

-- One-time initial load: project the whole model into the buffer with a
-- single set_lines and build the complete row map. Live batches after this
-- are incremental (the three operations above). An assistant_message with
-- attached blocks is folded at its arrival position — label, then each
-- attached block's lines in order, then the am's content — with sub-entries
-- for every block (blocks are skipped by the outer loop). The model stores
-- the am's content as one field, so an interleaved content/block split
-- cannot be reconstructed at load time; the fold matches the common case
-- exactly (thinking runs precede the response text).
local function render_full_projection(model, state, ctx)
  local buf = ctx.buf
  local all_lines = {}
  local row = 0
  for _, el in ipairs(model.elements) do
    if state.host[el.id] then
      -- Attached block: already folded into its am's region above (the am
      -- always precedes its blocks in the model).
    else
      local el_lines
      if el.type == 'assistant_message' then
        local block_entries
        el_lines, block_entries = fold_am_blocks(model, ctx, el)
        set_element_row(state, el, row, #el_lines)
        if next(block_entries) then
          local list = state.attach[el.id]
          if not list then list = {}; state.attach[el.id] = list end
          for bid, info in pairs(block_entries) do
            set_element_row(state, model.by_id[bid], row + info.start_row, info.height)
            state.host[bid] = el.id
            list[#list + 1] = bid
          end
        end
      else
        el_lines = project_element(el, ctx)
        if not el_lines or #el_lines == 0 then
          set_element_row(state, el, nil, 0)
        else
          set_element_row(state, el, row, #el_lines)
        end
      end
      if el_lines and #el_lines > 0 then
        for _, l in ipairs(el_lines) do
          all_lines[#all_lines + 1] = l
        end
        row = row + #el_lines
      end
    end
  end
  if #all_lines > 0 then
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, all_lines)
    state.first_event = false
    for _, el in ipairs(model.elements) do
      local entry = state.rows[el.id]
      if entry and entry.start_row then
        if el.type == 'assistant_message' and state.attach[el.id] and #state.attach[el.id] > 0 then
          -- Folded am: only its chrome (the label) is highlighted here; each
          -- attached block is highlighted at its own sub-entry in this loop.
          for _, sp in ipairs(element_chrome_spans(el, ctx)) do
            add_tracked_col_highlight(state, el, buf, ctx.ns, sp.group, entry.start_row + sp.row, sp.start_col, sp.end_col)
          end
        else
          apply_element_highlights(state, buf, ctx.ns, el, ctx, entry.start_row, project_element(el, ctx))
        end
      end
    end
  end
end

-- Apply ONE diff to the buffer: order updated_all -> updated_content -> added.
-- A single event's diff never has two kinds touching the same element, so the
-- order is exact. Callers are responsible for the modifiable window and for
-- fill_ctx (render / render_batch do both).
local function apply_diff(model, diff, ctx)
  if not diff then return end
  local buf = ctx.buf
  if not vim.api.nvim_buf_is_valid(buf) then return end
  local state = get_renderer_state(model)
  for _, el in ipairs(diff.updated_all) do
    render_rebuild(model, el, state, ctx)
  end
  for _, entry in ipairs(diff.updated_content) do
    render_updated_content(model, entry, state, ctx)
  end
  for _, el in ipairs(diff.added) do
    render_added(model, el, state, ctx)
  end
end

-- Render a single diff inside one modifiable window (nested-safe: callers may
-- already be inside a window). No auto-scroll, no force_render_markdown —
-- render_batch owns those. A bulk context performs the one-time full
-- projection (the initial load batch).
local function render(model, diff, ctx)
  if not diff then return end
  local buf = ctx.buf
  if not vim.api.nvim_buf_is_valid(buf) then return end
  fill_ctx(ctx)
  with_modifiable(buf, function()
    local state = get_renderer_state(model)
    if ctx.bulk and state.first_event then
      render_full_projection(model, state, ctx)
    else
      apply_diff(model, diff, ctx)
    end
  end)
end

-- Apply an ORDERED LIST of per-event diffs (one apply per event) in a single
-- modifiable window. Computes was_at_bottom BEFORE any writes; after the
-- window, if the cursor was at the bottom, moves it to the end of the last
-- line so the viewport follows the stream. Kicks force_render_markdown once
-- per batch. A failing diff stops the batch (reported, not raised). The first
-- batch (bulk = true) performs the one-time full projection instead of
-- incremental applies.
local function render_batch(model, diffs, ctx)
  if not diffs or #diffs == 0 then return end
  local buf = ctx.buf
  if not vim.api.nvim_buf_is_valid(buf) then return end

  local win = vim.fn.bufwinid(buf)
  local was_at_bottom = false
  if win ~= -1 then
    local cursor_line = vim.api.nvim_win_get_cursor(win)[1]
    local line_count = vim.api.nvim_buf_line_count(buf)
    was_at_bottom = cursor_line >= line_count
  end

  fill_ctx(ctx)
  with_modifiable(buf, function()
    local state = get_renderer_state(model)
    if ctx.bulk and state.first_event then
      render_full_projection(model, state, ctx)
    else
      for _, diff in ipairs(diffs) do
        local ok, err = pcall(apply_diff, model, diff, ctx)
        if not ok then
          vim.api.nvim_err_writeln('render error: ' .. tostring(err))
          break
        end
      end
    end
  end)

  if win ~= -1 and was_at_bottom then
    local last_line_nr = vim.api.nvim_buf_line_count(buf)
    local last_line_text = vim.api.nvim_buf_get_lines(buf, last_line_nr - 1, last_line_nr, false)[1] or ''
    pcall(vim.api.nvim_win_set_cursor, win, { last_line_nr, #last_line_text })
  end

  force_render_markdown(buf)
end

-- Resolve the element whose projected region covers the given 0-indexed row:
-- scan elements from the END (the most recent element wins), skip zero-height
-- elements, return (element, offset) with offset = row - start_row. Pure
-- integer lookup on the row map.
local function row_element_at(model, row)
  local state = get_renderer_state(model)
  for i = #model.elements, 1, -1 do
    local el = model.elements[i]
    local entry = state.rows[el.id]
    local start_row = entry and entry.start_row
    if start_row then
      local height = entry.height or 1
      if row >= start_row and row < start_row + height then
        return el, row - start_row
      end
    end
  end
  return nil, nil
end

-- Resolve the element under a buffer row as (element, offset) via the integer
-- row map. `buf` is unused — kept for signature stability with callers that
-- predate the row map.
local function element_at_row(model, buf, row)
  return row_element_at(model, row)
end

-- Compat wrappers for the reducer toggles. These are test-facing; each one
-- applies the reducer operation and renders the resulting diff into buf.
-- Phase 5's migrated suites call them with (model, element, buf, ns).
local function collapse_thinking(model, el, buf, ns)
  local d = collapse_open_thinking(model)
  if #d.updated_all > 0 then
    render(model, d, { buf = buf, ns = ns, bulk = false })
  end
end

local function toggle_thinking(model, el, buf, ns)
  if el and el.type == 'thinking_block' then
    local d = toggle_thinking_element(model, el)
    render(model, d, { buf = buf, ns = ns, bulk = false })
  end
end

-- Apply one display event through the model + renderer: build the render ctx
-- (width + media_root) and dispatch to render_batch (its own modifiable
-- window, scroll follow, and markdown kick). Kept as a thin compat wrapper
-- (the reader and keymaps now call apply/render_batch directly).
local function render_event(buf, ns, event, envelope_id, bulk)
  local diff = apply(model, event, envelope_id)
  render_batch(model, { diff }, { buf = buf, ns = ns, bulk = bulk })
end

-- Set up highlight groups used by all display buffers
local function setup_highlights(statusline_fg, statusline_ctermfg)
  vim.api.nvim_set_hl(0, 'TCodeUser', { fg = '#61afef', bold = true, ctermfg = 75 })
  vim.api.nvim_set_hl(0, 'TCodeAssistant', { fg = '#98c379', bold = true, ctermfg = 114 })
  vim.api.nvim_set_hl(0, 'TCodeTool', { fg = '#e5c07b', bold = true, ctermfg = 180 })
  vim.api.nvim_set_hl(0, 'TCodeThinking', { fg = '#7c8495', italic = true, ctermfg = 245 })
  vim.api.nvim_set_hl(0, 'TCodeToolArgs', { fg = '#7c8495', italic = true, ctermfg = 245 })
  vim.api.nvim_set_hl(0, 'TCodeTokens', { fg = '#5c6370', italic = true, ctermfg = 242 })
  vim.api.nvim_set_hl(0, 'TCodeSuccess', { fg = '#98c379', bold = true, ctermfg = 114 })
  vim.api.nvim_set_hl(0, 'TCodeError', { fg = '#e06c75', bold = true, ctermfg = 168 })
  vim.api.nvim_set_hl(0, 'TCodePermission', { fg = '#e5c07b', bold = true, ctermfg = 11 })
  vim.api.nvim_set_hl(0, 'TCodeSystemInfo', { fg = '#61afef', italic = true, ctermfg = 75 })
  vim.api.nvim_set_hl(0, 'TCodeSystemWarning', { fg = '#e5c07b', bold = true, ctermfg = 180 })
  vim.api.nvim_set_hl(0, 'TCodeSystemError', { fg = '#e06c75', bold = true, ctermfg = 168 })
  vim.api.nvim_set_hl(0, 'TCodeStatusLine', {
    bg = '#282c34', fg = statusline_fg,
    ctermfg = statusline_ctermfg, ctermbg = 236,
  })
end

local function disable_conflicting_plugins()
  -- Disable known statusline plugins and kill their autocmds so they
  -- cannot re-assert. Supported: lualine, vim-airline, lightline.
  pcall(function()
    require('lualine').hide()
    vim.api.nvim_del_augroup_by_name('lualine')
  end)
  pcall(function()
    vim.cmd('AirlineToggle')
    vim.api.nvim_del_augroup_by_name('airline')
  end)
  pcall(function()
    vim.fn['lightline#disable']()
    vim.api.nvim_del_augroup_by_name('lightline')
  end)
  -- Wipe dashboard/start screen buffers created before us
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    local ft = vim.bo[buf].filetype
    if ft == 'alpha' or ft == 'dashboard' or ft == 'snacks_dashboard' or ft == 'starter' then
      pcall(vim.api.nvim_buf_delete, buf, { force = true })
    end
  end
end

-- Create a read-only display buffer with standard options
-- @return buf number
local function create_display_buffer(name, statusline)
  vim.cmd('enew')
  vim.api.nvim_buf_set_name(0, name)

  vim.bo.buftype = 'nofile'
  vim.bo.bufhidden = 'hide'
  vim.bo.swapfile = false
  vim.bo.modifiable = false

  vim.wo.wrap = true
  vim.wo.linebreak = true
  vim.wo.number = false
  vim.wo.relativenumber = false
  vim.wo.signcolumn = 'no'
  vim.wo.statusline = statusline

  return vim.api.nvim_get_current_buf()
end

-- Create an incremental JSONL file reader that tracks position and buffers partial lines.
-- Returns a reader table and a check() function.
-- @param filepath: path to the JSONL file
-- @param buf: buffer to render into
-- @param ns: extmark namespace
-- @param on_event: optional callback(variant, data) called for each decoded event before rendering
local function create_jsonl_reader(filepath, buf, ns, on_event)
  local state = { last_size = 0, line_buffer = '', is_initial_load = true }

  local function check()
    local file = io.open(filepath, 'r')
    if not file then return end
    file:seek('set', state.last_size)
    local new_content = file:read('*all')
    file:close()

    if not new_content or #new_content == 0 then return end
    state.last_size = state.last_size + #new_content

    local data = state.line_buffer .. new_content
    local lines = vim.split(data, '\n', { plain = true })
    if data:sub(-1) ~= '\n' then
      state.line_buffer = lines[#lines]
      table.remove(lines, #lines)
    else
      state.line_buffer = ''
    end

    vim.schedule(function()
      if not vim.api.nvim_buf_is_valid(buf) then return end

      -- Apply each event to the model, collecting the per-event diffs in
      -- order; a failing apply stops the rest of this batch.
      local diffs = {}
      for _, line in ipairs(lines) do
        if line ~= '' then
          local ok, event = pcall(vim.json.decode, line)
          if ok and event then
            -- Capture the envelope id (pinned reference to this display event)
            -- BEFORE unwrapping, so `gb` can target the exact user message.
            -- Legacy lines have no top-level id; envelope_id stays nil.
            local envelope_id = nil
            if type(event) == 'table' and event.id ~= nil then
              envelope_id = event.id
            end
            -- New wire format: {"id": N, "msg": {"Variant": {...}}}. Unwrap
            -- to the legacy {"Variant": {...}} shape the renderers expect.
            -- Legacy lines have no top-level "msg" key and pass through.
            if type(event) == 'table' and type(event.msg) == 'table' then
              event = event.msg
            end
            if on_event then
              local variant, event_data = next(event)
              local ev_ok, ev_err = pcall(on_event, variant, event_data)
              if not ev_ok then
                vim.api.nvim_err_writeln('on_event error: ' .. tostring(ev_err))
              end
            end
            local a_ok, diff = pcall(apply, model, event, envelope_id)
            if not a_ok then
              vim.api.nvim_err_writeln('apply error: ' .. tostring(diff))
              break
            end
            diffs[#diffs + 1] = diff
          end
        end
      end

      -- Render the whole batch inside render_batch's single modifiable window;
      -- the first batch (bulk = true) performs the one-time full projection.
      render_batch(model, diffs, { buf = buf, ns = ns, bulk = state.is_initial_load })
      if state.is_initial_load then
        state.is_initial_load = false
      end
    end)
  end

  return check
end

-- Watch a status file and call on_status(content) when it changes
local function create_status_watcher(filepath, on_status)
  return watch_file(filepath, function()
    local file = io.open(filepath, 'r')
    if not file then return end
    local status = file:read('*all')
    file:close()
    if status and status ~= '' then
      vim.schedule(function()
        on_status(status)
      end)
    end
  end)
end

-- Last message from open_pending_approvals, for re-echo after startinsert
local last_approval_msg = nil

-- Open pending tool approvals via tcode approve-next CLI
local function open_pending_approvals()
  last_approval_msg = nil
  if not M.exe_path or not M.session_id then
    last_approval_msg = 'Session info not available'
    vim.notify(last_approval_msg, vim.log.levels.ERROR)
    return
  end
  local result = vim.fn.system(string.format(
    '%s --session=%s approve-next', shquote(M.exe_path), shquote(M.session_id)))
  local trimmed = vim.trim(result)
  if trimmed ~= '' then
    last_approval_msg = trimmed
    vim.notify(trimmed, vim.log.levels.INFO, { title = 'TCode' })
  end
end

-- Keymap handler bodies, factored out of setup_display so the test runner can
-- invoke the exact production logic. Each takes the display's (model, buf,
-- ns); the keymap closures below delegate to them, so behavior is identical.

-- `o`: toggle a thinking block, or open the subagent / tool-call detail view
-- from ANY row of a tool / subagent element. Rows resolve through the
-- integer row map (element_at_row) and the pure action_at lookup.
local function keymap_o(model, buf, ns)
  local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed

  local el = select(1, element_at_row(model, buf, cursor_line))
  if not el then return end
  local kind = action_at(el)

  if kind == 'thinking' then
    local d = toggle_thinking_element(model, el)
    if #d.updated_all > 0 then
      render(model, d, { buf = buf, ns = ns, bulk = false })
    end
    return
  elseif kind == 'detail' then
    -- The detail view opens from any row of the element. A pending subagent
    -- (nil conversation_id) is a silent no-op.
    if el.type == 'subagent' then
      if not el.conversation_id then return end
      if not M.exe_path or not M.session_id then
        vim.notify('Session info not available', vim.log.levels.ERROR)
        return
      end
      vim.fn.system(string.format('%s --session=%s open-subagent %s',
        shquote(M.exe_path), shquote(M.session_id), shquote(el.conversation_id)))
      return
    end
    if el.type == 'tool_call' and el.tool_call_id then
      if not M.exe_path or not M.session_id then
        vim.notify('Session info not available', vim.log.levels.ERROR)
        return
      end
      vim.fn.system(string.format('%s --session=%s open-tool-call %s',
        shquote(M.exe_path), shquote(M.session_id), shquote(el.tool_call_id)))
      return
    end
  end
  -- Otherwise nothing under the cursor to act on.
end

-- `<C-k>`: cancel tool or subagent with confirmation popup.
local function keymap_ck(model, buf)
  if not M.exe_path or not M.session_id then
    vim.notify('Session info not available', vim.log.levels.ERROR)
    return
  end

  local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed
  local el, _ = element_at_row(model, buf, cursor_line)
  if not el then
    vim.notify('No tool call or subagent under cursor', vim.log.levels.WARN)
    return
  end

  if el.type == 'subagent' and el.conversation_id then
    local final = el.status == 'done' or el.status == 'failed'
      or el.status == 'cancelled' or el.status == 'denied'
    if final then
      vim.notify('Subagent already finished', vim.log.levels.INFO)
      return
    end
    -- desc is wire-derived and can contain '\n', which confirm_popup cannot
    -- write as one buffer line: collapse newlines before building the prompt.
    local desc = single_line(el.description or el.conversation_id)
    confirm_popup("Cancel subagent '" .. desc .. "'? (y/n)", function()
      local cmd = string.format('%s --session=%s cancel-conversation %s',
        shquote(M.exe_path), shquote(M.session_id), shquote(el.conversation_id))
      local result = vim.fn.system(cmd)
      vim.notify(vim.trim(result), vim.log.levels.INFO, { title = 'TCode' })
    end)
  elseif el.type == 'tool_call' and el.tool_call_id then
    local final = el.status == 'done' or el.status == 'failed'
      or el.status == 'cancelled' or el.status == 'denied'
    if final then
      vim.notify('Tool call already finished', vim.log.levels.INFO)
      return
    end
    confirm_popup("Cancel tool '" .. single_line(el.tool_name or 'unknown') .. "'? (y/n)", function()
      local cmd = string.format('%s --session=%s cancel-tool %s',
        shquote(M.exe_path), shquote(M.session_id), shquote(el.tool_call_id))
      local result = vim.fn.system(cmd)
      vim.notify(vim.trim(result), vim.log.levels.INFO, { title = 'TCode' })
    end)
  else
    vim.notify('No tool call or subagent under cursor', vim.log.levels.WARN)
  end
end

-- `gb`: branch the conversation at the user message under the cursor.
local function keymap_gb(model, buf)
  if not M.exe_path or not M.session_id then
    vim.notify('Session info not available', vim.log.levels.ERROR)
    return
  end
  local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed
  local el, _ = element_at_row(model, buf, cursor_line)
  if not (el and el.type == 'user_message' and el.msg_id) then
    vim.notify('not on a user message', vim.log.levels.WARN)
    return
  end
  local profile_part = ''
  if M.profile and M.profile ~= '' then
    -- Single-quote-escape so a profile with shell metacharacters is never
    -- interpreted by the shell that runs the CLI command.
    profile_part = ' -p ' .. shquote(M.profile)
  end
  local cmd = string.format('%s%s --session=%s branch %s',
    shquote(M.exe_path), profile_part, shquote(M.session_id), shquote(el.msg_id))
  local result = vim.fn.system(cmd)
  local trimmed = vim.trim(result)
  if trimmed ~= '' then
    local level = vim.v.shell_error ~= 0 and vim.log.levels.ERROR or vim.log.levels.INFO
    vim.notify(trimmed, level, { title = 'TCode' })
  end
end

-- Setup display window for viewing conversation
-- @param display_file: Path to file where display content is written (JSONL)
-- @param status_file: Path to file where status messages are written
-- @param usage_file: Path to file where subscription usage is written
-- @param token_usage_file: Path to file where token usage is written
-- @param session_id: Session ID for spawning tool call windows
-- @param exe_path: Path to tcode executable
-- @param parser_path: Path to libtree_sitter_tcode.so/.dylib (optional, for treesitter isolation)
-- @param runtime_dir: Root directory containing queries/tcode/*.scm (optional, prepended to runtimepath)
function M.setup_display(display_file, status_file, usage_file, token_usage_file, session_id, exe_path, parser_path, runtime_dir, effort_file, is_subagent, profile)
  M.display_file = display_file or '/tmp/tcode-display.jsonl'
  M.status_file = status_file or '/tmp/tcode-status.txt'
  M.usage_file = usage_file
  M.token_usage_file = token_usage_file
  M.effort_file = effort_file
  M.session_id = session_id
  M.exe_path = exe_path
  M.profile = profile

  vim.g.tcode_status = 'Connecting...'
  vim.g.tcode_usage = ''
  vim.g.tcode_token_usage = ''
  vim.g.tcode_combined_usage = ''
  vim.g.tcode_effort = ''

  local function update_combined_usage()
    local parts = {}
    if vim.g.tcode_token_usage ~= '' then table.insert(parts, vim.g.tcode_token_usage) end
    if vim.g.tcode_usage ~= '' then table.insert(parts, vim.g.tcode_usage) end
    vim.g.tcode_combined_usage = table.concat(parts, ' │ ')
  end

  setup_highlights('#98c379', 114)
  disable_conflicting_plugins()
  local buf = create_display_buffer('tcode',
    '%#TCodeStatusLine# TCode: %{g:tcode_status} | Reasoning effort: %{g:tcode_effort}%=%{g:tcode_combined_usage} ')
  local ns = vim.api.nvim_create_namespace('tcode')

  -- Mark the buffer as tcode so our custom tree-sitter grammar handles separator
  -- lines and injects each content region as independent markdown parses.
  vim.bo[buf].filetype = 'tcode'

  -- Setting filetype above synchronously fires the FileType autocmd, which
  -- causes render-markdown.nvim (if installed and configured for `tcode`)
  -- to attach and populate its per-buffer config cache with the default
  -- 100ms debounce. Override that debounce to 0 for this buffer so streaming
  -- inserts don't get rate-limited away by the plugin's trailing-edge
  -- debounce. See force_render_markdown for the full explanation. Markdown
  -- buffers in other windows are unaffected.
  --
  -- Compatibility notes:
  --   - render-markdown.nvim NOT installed: set_render_markdown_debounce
  --     and the per-batch force_render_markdown call from the JSONL reader
  --     are both pcall-guarded silent no-ops. Fence concealment, if any,
  --     comes from nvim's built-in tree-sitter highlighter via the
  --     markdown injection — which has no debounce of its own and renders
  --     synchronously during the redraw cycle, so the bug this hack works
  --     around does not apply.
  --   - Other markdown rendering plugins (markview.nvim, headlines.nvim,
  --     noice.nvim, etc.) are NOT specifically integrated. If they have
  --     a similar trailing-edge debounce on their own update path, the
  --     same symptom may appear and would need a separate fix wired in
  --     here against that plugin's API.
  set_render_markdown_debounce(buf, 0)

  -- Reset the model for this display session (the fresh weak-keyed renderer
  -- state starts with first_event = true; the explicit reset is belt and
  -- suspenders).
  model = new_model()
  get_renderer_state(model).first_event = true

  -- Register tcode tree-sitter parser and start highlighting
  if parser_path and parser_path ~= '' then
    local ok, err = pcall(vim.treesitter.language.add, 'tcode', { path = parser_path })
    if ok then
      if runtime_dir and runtime_dir ~= '' then
        vim.opt.runtimepath:prepend(runtime_dir)
      end
      pcall(vim.treesitter.start, buf, 'tcode')
    else
      vim.notify('tcode: tree-sitter parser not loaded: ' .. tostring(err), vim.log.levels.WARN)
    end
  end

  local bell_pending = false
  local bell_enabled = false
  local check_updates = create_jsonl_reader(M.display_file, buf, ns, function(variant, event_data)
    if variant == 'AssistantMessageStart' then
      bell_pending = true
    elseif variant == 'AssistantMessageEnd' then
      if bell_enabled and bell_pending and (event_data.tool_call_count or 0) == 0 then
        os.execute('printf "\\a"')
      end
      bell_pending = false
    end
  end)
  M.display_watcher = watch_file(M.display_file, check_updates)
  vim.schedule(function() bell_enabled = true end)
  M.status_watcher = create_status_watcher(M.status_file, function(status)
    if status == 'Shutdown' then
      vim.cmd('qa!')
      return
    end
    vim.g.tcode_status = status
    vim.cmd('redrawstatus')
  end)

  -- Watch usage file for subscription usage updates.
  -- The file is pre-created by the Rust side before nvim starts.
  if M.usage_file then
    M.usage_watcher = create_status_watcher(M.usage_file, function(usage)
      if usage and usage ~= '' then
        vim.g.tcode_usage = usage
      else
        vim.g.tcode_usage = ''
      end
      update_combined_usage()
      vim.cmd('redrawstatus')
    end)
  end

  -- Watch token usage file for token count updates.
  -- The file is pre-created by the Rust side before nvim starts.
  if M.token_usage_file then
    M.token_usage_watcher = create_status_watcher(M.token_usage_file, function(token_usage)
      if token_usage and token_usage ~= '' then
        vim.g.tcode_token_usage = token_usage
      else
        vim.g.tcode_token_usage = ''
      end
      update_combined_usage()
      vim.cmd('redrawstatus')
    end)
  end

  -- Watch effort file for reasoning effort updates.
  if M.effort_file then
    M.effort_watcher = create_status_watcher(M.effort_file, function(effort)
      if effort and effort ~= '' then
        vim.g.tcode_effort = effort
      else
        vim.g.tcode_effort = ''
      end
      update_combined_usage()
      vim.cmd('redrawstatus')
    end)
  end

  -- Clean up watchers when buffer is deleted or wiped
  vim.api.nvim_create_autocmd({'BufDelete', 'BufWipeout'}, {
    buffer = buf,
    callback = function()
      if M.display_watcher then M.display_watcher.stop(); M.display_watcher = nil end
      if M.status_watcher then M.status_watcher.stop(); M.status_watcher = nil end
      if M.usage_watcher then M.usage_watcher.stop(); M.usage_watcher = nil end
      if M.token_usage_watcher then M.token_usage_watcher.stop(); M.token_usage_watcher = nil end
      if M.effort_watcher then M.effort_watcher.stop(); M.effort_watcher = nil end
    end,
  })

  if is_subagent then
    vim.keymap.set('n', 'q', ':qa!<CR>', { buffer = true, silent = true, desc = 'Quit' })
  else
    vim.keymap.set('n', 'q', function()
      confirm_popup("Cancel and exit conversation? (y/n)", function()
        vim.cmd('qa!')
      end)
    end, { buffer = true, silent = true, desc = 'Quit' })
  end

  -- Context-aware 'o' keybinding: toggle a thinking block, or open the
  -- subagent / tool-call detail view from any row of a tool / subagent
  -- element. Rows resolve through the integer row map (element_at_row) and
  -- the pure action_at lookup (see keymap_o).
  vim.keymap.set('n', 'o', function()
    keymap_o(model, buf, ns)
  end, { buffer = true, silent = true, desc = 'Toggle thinking or open detail' })

  -- Cancel tool or subagent with confirmation popup (Ctrl-k) — see keymap_ck.
  vim.keymap.set('n', '<C-k>', function()
    keymap_ck(model, buf)
  end, { buffer = true, silent = true, desc = 'Cancel tool or subagent' })

  -- Cancel entire conversation with confirmation popup (Ctrl-C)
  vim.keymap.set('n', '<C-c>', function()
    if not M.exe_path or not M.session_id then
      vim.notify('Session info not available', vim.log.levels.ERROR)
      return
    end

    -- Read conversation ID from conversation-state.json in the session directory
    local session_dir = vim.fn.fnamemodify(M.display_file, ':h')
    local state_file = session_dir .. '/conversation-state.json'
    local f = io.open(state_file, 'r')
    if not f then
      vim.notify('Cannot read conversation state', vim.log.levels.ERROR)
      return
    end
    local content = f:read('*a')
    f:close()
    local ok, data = pcall(vim.json.decode, content)
    if not ok or not data or not data.id then
      vim.notify('Cannot parse conversation state', vim.log.levels.ERROR)
      return
    end
    local conv_id = data.id

    confirm_popup("Cancel conversation? (y/n)", function()
      local cmd = string.format('%s --session=%s cancel-conversation %s',
        shquote(M.exe_path), shquote(M.session_id), shquote(conv_id))
      local result = vim.fn.system(cmd)
      vim.notify(vim.trim(result), vim.log.levels.INFO, { title = 'TCode' })
    end)
  end, { buffer = true, silent = true, desc = 'Cancel conversation' })

  -- Open pending tool approvals (Ctrl-P)
  vim.keymap.set('n', '<C-p>', open_pending_approvals,
    { buffer = true, silent = true, desc = 'Open pending tool approvals' })

  -- Branch the conversation at the user message under the cursor (gb) — see
  -- keymap_gb.
  if not is_subagent then
    vim.keymap.set('n', 'gb', function()
      keymap_gb(model, buf)
    end, { buffer = true, silent = true, desc = 'Branch conversation at user message' })
  end
end

-- Setup tool call display window for viewing a single tool call's details
-- @param tool_call_file: Path to the per-tool-call JSONL file
-- @param status_file: Path to the per-tool-call status file
function M.setup_tool_call_display(tool_call_file, status_file)
  M.tc_file = tool_call_file
  M.tc_status_file = status_file
  -- Fresh model with full_input set: the detail view never collapses args.
  model = new_model()
  model.full_input = true
  get_renderer_state(model).first_event = true

  vim.g.tcode_tc_status = 'Waiting...'

  setup_highlights('#e5c07b', 180)
  disable_conflicting_plugins()
  local buf = create_display_buffer('tcode-tool-call',
    '%#TCodeStatusLine# Tool Call: %{g:tcode_tc_status} %=')
  local ns = vim.api.nvim_create_namespace('tcode_tc')

  local check_updates = create_jsonl_reader(M.tc_file, buf, ns, function(variant, data)
    if variant == 'AssistantToolCallStart' then
      vim.g.tcode_tc_status = 'Generating: ' .. (data.tool_name or '')
      vim.cmd('redrawstatus')
    elseif variant == 'ToolMessageStart' then
      vim.g.tcode_tc_status = 'Running: ' .. (data.tool_name or '')
      vim.cmd('redrawstatus')
    elseif variant == 'ToolMessageEnd' then
      vim.g.tcode_tc_status = 'Done: ' .. (data.end_status or 'Unknown')
      vim.cmd('redrawstatus')
    end
  end)

  M.tc_watcher = watch_file(M.tc_file, check_updates)
  M.tc_status_watcher = create_status_watcher(M.tc_status_file, function()
    vim.cmd('redrawstatus')
  end)

  -- Clean up watchers when buffer is deleted
  vim.api.nvim_create_autocmd('BufDelete', {
    buffer = buf,
    callback = function()
      if M.tc_watcher then M.tc_watcher.stop(); M.tc_watcher = nil end
      if M.tc_status_watcher then M.tc_status_watcher.stop(); M.tc_status_watcher = nil end
    end,
  })

  vim.keymap.set('n', 'q', ':qa!<CR>', { buffer = true, silent = true, desc = 'Quit' })
end

-- Setup edit window for composing messages
-- Load user-invocable skill templates (injected by Rust via _G.tcode_skills and _G.tcode_skill_descriptions)
-- Returns two tables: skills (skill_name -> body_text), descriptions (skill_name -> description)
-- Returns empty tables if no skills configured.
local function load_user_skills()
  return _G.tcode_skills or {}, _G.tcode_skill_descriptions or {}
end

-- Attempt to expand a /skill at the cursor position.
-- @param skills: table of skill_name -> template_text
-- @param cursor_col: optional 0-indexed byte column (uses current cursor if nil)
-- Returns true if expanded, false otherwise.
local function try_expand_skill(skills, cursor_col)
  local line = vim.api.nvim_get_current_line()
  local cursor = vim.api.nvim_win_get_cursor(0)
  local row = cursor[1]  -- 1-indexed
  local col = cursor_col or cursor[2]  -- 0-indexed byte position

  -- Find /command ending at or before cursor position
  local before_cursor = line:sub(1, col)
  local cmd_start, _, cmd = before_cursor:find('/([%w%-_]+)%s*$')

  if not cmd then
    return false
  end

  local template = skills[cmd]
  if not template then
    return false
  end

  -- Text before the /command and after cursor
  local prefix = line:sub(1, cmd_start - 1)
  local suffix = line:sub(col + 1)

  -- Split template into lines
  local replacement_lines = {}
  local pos = 1
  while true do
    local finish = template:find('\n', pos, true)
    if not finish then
      table.insert(replacement_lines, template:sub(pos))
      break
    end
    table.insert(replacement_lines, template:sub(pos, finish - 1))
    pos = finish + 1
  end

  -- Combine with surrounding text
  replacement_lines[1] = prefix .. replacement_lines[1]
  replacement_lines[#replacement_lines] = replacement_lines[#replacement_lines] .. suffix

  -- Replace the current line with the replacement lines
  vim.api.nvim_buf_set_lines(0, row - 1, row, false, replacement_lines)

  -- Move cursor to end of expanded template (before suffix)
  local last_row = row - 1 + #replacement_lines
  local final_col = #replacement_lines[#replacement_lines] - #suffix
  vim.api.nvim_win_set_cursor(0, { last_row, final_col })

  return true
end

-- Set up completion function for /skills.
-- Called by nvim's insert-mode completion (<C-x><C-u>).
-- We wire <Tab> to trigger this when appropriate.
-- @param skills: table of skill_name -> body_text
-- @param descriptions: table of skill_name -> description
local function setup_skill_completion(skills, descriptions)
  -- Build sorted list of skill names for stable ordering
  local skill_names = {}
  for name, _ in pairs(skills) do
    table.insert(skill_names, name)
  end
  table.sort(skill_names)

  -- Register the completefunc
  -- completefunc is called twice by nvim:
  --   1st call (findstart=1): return the column where the completion word starts
  --   2nd call (findstart=0): return the list of matches for `base`
  _G.tcode_skill_complete = function(findstart, base)
    if findstart == 1 then
      -- Find the start of the /command on the current line
      local line = vim.api.nvim_get_current_line()
      local col = vim.fn.col('.') - 1  -- 0-indexed cursor column
      -- Walk backwards to find the '/'
      local start = col
      while start > 0 and line:sub(start, start):match('[%w%-_]') do
        start = start - 1
      end
      -- Check if we landed on a '/'
      if start >= 1 and line:sub(start, start) == '/' then
        -- Return 0-indexed column of the '/' character
        return start - 1
      end
      -- No '/' found — abort completion
      return -3
    else
      -- Return matching skills (base includes the '/')
      local prefix = base:match('^/(.*)') or ''
      local matches = {}
      for _, name in ipairs(skill_names) do
        if name:find(prefix, 1, true) == 1 then
          table.insert(matches, {
            word = '/' .. name,
            menu = descriptions[name] or '',
          })
        end
      end
      return matches
    end
  end

  vim.bo.completefunc = 'v:lua.tcode_skill_complete'
  -- Don't auto-select first entry — let user continue typing to filter
  vim.opt_local.completeopt = { 'menu', 'menuone', 'noselect' }
end

-- @param msg_file: Path to file where messages should be written
-- @param is_subagent: Whether this is a subagent edit window
-- @param session_id: Session ID (for approve-next)
-- @param exe_path: Path to tcode executable (for approve-next)
function M.setup_edit(msg_file, is_subagent, session_id, exe_path)
  M.msg_file = msg_file or '/tmp/tcode-edit-msg.txt'
  M.session_id = session_id or M.session_id
  M.exe_path = exe_path or M.exe_path

  vim.cmd('enew')
  vim.api.nvim_buf_set_name(0, 'tcode-edit')
  disable_conflicting_plugins()

  vim.bo.buftype = 'acwrite'
  vim.bo.bufhidden = 'hide'
  vim.bo.swapfile = false
  vim.bo.filetype = 'markdown'

  vim.wo.wrap = true
  vim.wo.linebreak = true

  if is_subagent then
    vim.wo.statusline = '%#TCodeEditStatus# Subagent Edit - Enter to send, /done to finish %='
  else
    vim.wo.statusline = '%#TCodeEditStatus# TCode Edit - Enter to send, Ctrl-j new line, Ctrl-p approvals %='
  end

  -- Create autocmd to send content on save
  vim.api.nvim_create_autocmd('BufWriteCmd', {
    buffer = 0,
    callback = function()
      local buf = vim.api.nvim_get_current_buf()
      local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)

      local has_content = false
      for _, line in ipairs(lines) do
        if line:match('%S') and not line:match('^%-%-') then
          has_content = true
          break
        end
      end

      if has_content then
        local filtered_lines = {}
        for _, line in ipairs(lines) do
          if not line:match('^%-%-') then
            table.insert(filtered_lines, line)
          end
        end
        local filtered_content = table.concat(filtered_lines, '\n')

        local file = io.open(M.msg_file, 'w')
        if file then
          file:write(filtered_content)
          file:close()
          vim.api.nvim_buf_set_lines(buf, 0, -1, false, {})
        else
          vim.notify('Failed to send message', vim.log.levels.ERROR)
        end
      end

      vim.bo[buf].modified = false
    end,
  })

  vim.keymap.set('n', '<C-s>', ':w<CR>', { buffer = true, silent = true, desc = 'Send message' })
  vim.keymap.set('i', '<CR>', function()
    if vim.fn.pumvisible() == 1 then
      -- Completion popup visible — confirm selection (CompleteDone autocmd will auto-expand)
      vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<C-y>', true, false, true), 'n', false)
    else
      -- No popup — send message
      vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>:w<CR>i', true, false, true), 'n', false)
    end
  end, { buffer = true, silent = true, desc = 'Send message or confirm completion' })

  vim.cmd([[
    highlight TCodeEditStatus guibg=#282c34 guifg=#61afef ctermfg=75 ctermbg=236
  ]])

  -- Open pending tool approvals (Ctrl-P, normal and insert mode)
  vim.keymap.set('n', '<C-p>', open_pending_approvals,
    { buffer = true, silent = true, desc = 'Open pending tool approvals' })
  vim.keymap.set('i', '<C-p>', function()
    vim.cmd('stopinsert')
    open_pending_approvals()
    vim.schedule(function()
      vim.cmd('startinsert')
      if last_approval_msg then
        vim.o.showmode = false
        vim.defer_fn(function()
          vim.api.nvim_echo({{ last_approval_msg }}, false, {})
          vim.defer_fn(function() vim.o.showmode = true end, 2000)
        end, 50)
      end
    end)
  end, { buffer = true, silent = true, desc = 'Open pending tool approvals' })

  -- Load user-invocable skills
  local skills, descriptions = load_user_skills()

  -- Set up skill keybindings if skills are available
  if next(skills) ~= nil then
    setup_skill_completion(skills, descriptions)

    -- Auto-trigger completion popup when typing '/'
    vim.keymap.set('i', '/', function()
      local col = vim.fn.col('.') - 1  -- 0-indexed cursor column
      local line = vim.api.nvim_get_current_line()
      -- Trigger if at start of line or preceded by whitespace
      if col == 0 or line:sub(col, col):match('%s') then
        vim.api.nvim_feedkeys('/', 'n', false)
        vim.schedule(function()
          vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<C-x><C-u>', true, false, true), 'n', false)
        end)
      else
        vim.api.nvim_feedkeys('/', 'n', false)
      end
    end, { buffer = true, silent = true, desc = 'Auto-trigger skill completion' })

    -- <Tab> in insert mode: expand skill, show completion, or insert tab
    vim.keymap.set('i', '<Tab>', function()
      -- Check if completion popup is already visible — if so, select next item
      if vim.fn.pumvisible() == 1 then
        vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<C-n>', true, false, true), 'n', false)
        return
      end

      local line = vim.api.nvim_get_current_line()
      local col = vim.fn.col('.') - 1  -- 0-indexed cursor column
      local before_cursor = line:sub(1, col)
      local cmd = before_cursor:match('/([%w%-_]+)%s*$')

      if cmd and skills[cmd] then
        -- Exact match — expand the skill (pass col captured in insert mode)
        vim.cmd('stopinsert')
        try_expand_skill(skills, col)
        vim.cmd('startinsert')
      elseif before_cursor:match('/%s*$') or cmd then
        -- Has / with partial or no text after it — trigger completion popup
        vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<C-x><C-u>', true, false, true), 'n', false)
      else
        -- No skill context — insert a normal tab
        vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Tab>', true, false, true), 'n', false)
      end
    end, { buffer = true, silent = true, desc = 'Expand skill or insert tab' })

    -- Auto-expand skill after selecting from completion popup
    vim.api.nvim_create_autocmd('CompleteDone', {
      buffer = 0,
      callback = function()
        local completed = vim.v.completed_item
        if completed and completed.word and completed.word:match('^/') then
          -- Schedule expansion to run after the completion popup closes
          vim.schedule(function()
            try_expand_skill(skills)
          end)
        end
      end,
    })
  end

  vim.api.nvim_buf_set_lines(0, 0, -1, false, { '' })

  -- Check for LSP hint
  local session_dir = vim.fn.fnamemodify(msg_file, ':h')
  local hint_path = session_dir .. '/lsp-hint.txt'
  local hint_file = io.open(hint_path, 'r')
  if hint_file then
    local hint_lines = {}
    for line in hint_file:lines() do
      table.insert(hint_lines, line)
    end
    hint_file:close()

    if #hint_lines > 0 then
      vim.api.nvim_set_hl(0, 'TCodeTokens', { fg = '#5c6370', italic = true, ctermfg = 242 })
      local hint_ns = vim.api.nvim_create_namespace('tcode_lsp_hint')
      -- First line as overlay on line 0, additional lines as virtual lines below
      vim.api.nvim_buf_set_extmark(0, hint_ns, 0, 0, {
        virt_text = { { hint_lines[1], 'TCodeTokens' } },
        virt_text_pos = 'overlay',
        virt_lines = vim.tbl_map(function(line)
          return { { line, 'TCodeTokens' } }
        end, vim.list_slice(hint_lines, 2)),
      })

      -- Clear on first edit
      vim.api.nvim_create_autocmd({ 'InsertCharPre' }, {
        buffer = 0,
        once = true,
        callback = function()
          vim.api.nvim_buf_clear_namespace(0, hint_ns, 0, -1)
        end,
      })
    end
  end

  vim.cmd('startinsert')
end

return M
