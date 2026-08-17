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
-- (e.g. the 500ms settle flush, the `o` expand/collapse toggles) restore the
-- read-only display invariant.
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

-- Append text continuing from current buffer position (for streaming chunks)
local function append_text(buf, text)
  if not ensure_buf_modifiable(buf) then return end
  local line_count = vim.api.nvim_buf_line_count(buf)
  local last_line = vim.api.nvim_buf_get_lines(buf, line_count - 1, line_count, false)[1] or ''
  local lines = vim.split(text, '\n', { plain = true })
  vim.api.nvim_buf_set_text(buf, line_count - 1, #last_line, line_count - 1, #last_line, lines)
end

-- Namespace for tool-call range extmarks (per-element navigation ranges).
local tc_ns = vim.api.nvim_create_namespace('tcode_tc_id')

-- Namespace for subagent range extmarks (per-element navigation ranges).
local sa_ns = vim.api.nvim_create_namespace('tcode_sa_id')

-- Namespace for user-message range extmarks (`gb` branch targeting).
local um_ns = vim.api.nvim_create_namespace('tcode_um')

-- Namespace for per-element start-row anchor extmarks. Rows are resolved from
-- the extmarks at use time, never stored as stale integers.
local gen_ns = vim.api.nvim_create_namespace('tcode_gen')

-- Flag to handle the initial empty line in Neovim buffers
local first_event = true

-- Thinking indicator / expand-hint extmark namespace
local thinking_ns = vim.api.nvim_create_namespace('tcode_thinking')

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

--- Insert text at the end of a specific row, supporting multi-line text.
local function insert_text_at(buf, row, text)
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
-- element into one entry (the renderer appends per entry).
local function add_updated_content(diff, element, text)
  local uc = diff.updated_content
  local last = uc[#uc]
  if last and last[1] == element then
    last[2] = last[2] .. text
  else
    uc[#uc + 1] = { element, text }
  end
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

-- Estimated number of wrapped rows the text occupies at a reference width.
-- A pure proxy for the old width-aware visual check: tool/subagent args
-- arrive as single-logical-line JSON with escaped newlines, so a plain line
-- count would never collapse them even when they span many wrapped rows.
local ARGS_REF_WIDTH = 80

local function visual_lines(text)
  local total = 0
  local pos = 1
  while true do
    local finish = text:find('\n', pos, true)
    local line = finish and text:sub(pos, finish - 1) or text:sub(pos)
    total = total + math.max(1, math.ceil(#line / ARGS_REF_WIDTH))
    if not finish then break end
    pos = finish + 1
  end
  return total
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
    -- SubAgentContinue (exactly like find_pending_subagent). The settle flush
    -- closes the input fence mid-stream, so input_open alone would drop all
    -- later chunks: match pending subagents whose fence was already closed.
    if el.type == 'subagent' and el.tool_call_index == tool_call_index
      and (el.input_open or el.conversation_id == nil) then
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
  am.content = am.content .. pending
  add_updated_content(frag, am, pending)
  return frag
end

-- Collapse the open thinking block (structurally always the tail) to
-- 'collapsed'. Returns a full diff, empty when nothing was open.
local function collapse_open_thinking(model)
  local diff = new_diff()
  local tail = model.tail
  if tail and tail.type == 'thinking_block' and tail.state == 'open' then
    tail.state = 'collapsed'
    diff.updated_all[#diff.updated_all + 1] = tail
  end
  return diff
end

-- Settle-flush operation: close every open element (open thinking block, open
-- args/input fences), producing updated_all per element. Long args/input are
-- collapsed to a preview at the same time (matches the old flush behavior).
local function close_open_elements(model)
  local diff = merge_diff(new_diff(), collapse_open_thinking(model))
  for _, el in ipairs(model.elements) do
    if el.type == 'tool_call' and el.args_open then
      el.args_open = false
      if visual_lines(el.args) > 2 and not el.full_input then
        el.args_collapsed = true
      end
      diff.updated_all[#diff.updated_all + 1] = el
    elseif el.type == 'subagent' and el.input_open then
      el.input_open = false
      if visual_lines(el.input) > 2 then
        el.input_collapsed = true
      end
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

-- `o` toggle on a tool call (args preview) or a subagent (input preview):
-- flip the preview flag.
local function toggle_tool_call_args_element(model, element)
  local diff = new_diff()
  if element and element.type == 'tool_call' then
    element.args_collapsed = not element.args_collapsed
    diff.updated_all[#diff.updated_all + 1] = element
  elseif element and element.type == 'subagent' then
    element.input_collapsed = not element.input_collapsed
    diff.updated_all[#diff.updated_all + 1] = element
  end
  return diff
end

-- `o` toggle on a tool/subagent OUTPUT preview: flip the flag.
local function toggle_tool_output_element(model, element)
  local diff = new_diff()
  if element and (element.type == 'tool_call' or element.type == 'subagent') then
    element.output_collapsed = not element.output_collapsed
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
    if chunk == '' then return diff end -- never reopen/erase on an empty chunk
    local tail = model.tail
    if tail and tail.type == 'thinking_block' and tail.state == 'open' then
      -- Streaming into the open block.
      tail.content = tail.content .. chunk
      add_updated_content(diff, tail, chunk)
    elseif tail and tail.type == 'thinking_block' and tail.state == 'collapsed' then
      -- Merge: reopen the collapsed block in place. Held whitespace is
      -- discarded (today's merge swallows it). No element is ever removed.
      tail.state = 'open'
      tail.content = tail.content .. chunk
      model.pending_whitespace = nil
      diff.updated_all[#diff.updated_all + 1] = tail
    else
      -- New run: collapse any open thinking first, then open a fresh block.
      merge_diff(diff, collapse_open_thinking(model))
      local el = add_element(model, {
        type = 'thinking_block',
        content = chunk,
        state = 'open',
      })
      diff.added[#diff.added + 1] = el
      model.tail = el
    end

  elseif variant == 'AssistantMessageChunk' then
    local chunk = data.content or ''
    merge_diff(diff, collapse_open_thinking(model))
    if chunk == '' then return diff end
    if model.sa_active then
      -- Subagent output streams through AssistantMessageChunk after
      -- SubAgentStart: append to the active subagent's output, not the
      -- assistant message.
      local sa = find_subagent_by_conversation(model, model.sa_active)
      if sa then
        sa.output = sa.output .. chunk
        add_updated_content(diff, sa, chunk)
        return diff -- tail unchanged
      end
    end
    if is_whitespace_only(chunk) then
      -- Hold whitespace-only text: it must not move the tail away from a
      -- collapsed thinking block (the merge guard).
      model.pending_whitespace = (model.pending_whitespace or '') .. chunk
      return diff -- no diff entries for the chunk itself
    end
    merge_diff(diff, flush_pending_whitespace(model))
    local am = last_element_of_type(model, 'assistant_message')
    if not am then
      -- Defensive: AssistantMessageStart normally precedes, but a bare text
      -- chunk must still land somewhere.
      am = add_element(model, { type = 'assistant_message', content = '' })
      diff.added[#diff.added + 1] = am
    end
    am.content = am.content .. chunk
    add_updated_content(diff, am, chunk)
    model.tail = am

  elseif variant == 'AssistantMessageEnd' then
    merge_diff(diff, flush_pending_whitespace(model))
    -- Close every still-open args/input fence first, then collapse any open
    -- thinking (today's order).
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
      args_collapsed = false,
      output_started = false,
      output_open = false,
      output = '',
      output_collapsed = false,
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
      el.args = el.args .. content
      add_updated_content(diff, el, content)
    end
    -- missing mapping -> drop silently

  elseif variant == 'ToolMessageStart' then
    merge_diff(diff, collapse_open_thinking(model))
    local el = find_tool_call_by_id(model, data.tool_call_id)
    if el then
      -- Close the args fence, collapse long args to a preview, open output.
      el.args_open = false
      if visual_lines(el.args) > 2 and not el.full_input then
        el.args_collapsed = true
      end
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
        args_collapsed = false,
        output_started = true,
        output_open = true,
        output = '',
        output_collapsed = false,
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
      el.output = el.output .. content
      add_updated_content(diff, el, content)
    else
      -- Fallback: today appends at the buffer tail, which in the model is the
      -- assistant message when it is the tail; otherwise drop.
      local tail = model.tail
      if tail and tail.type == 'assistant_message' then
        local content = tostring(data.content)
        tail.content = tail.content .. content
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
      -- Long results auto-collapse to a preview (the detail view keeps them
      -- expanded via full_input).
      if visual_lines(el.output) > 2 and not el.full_input then
        el.output_collapsed = true
      end
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
      input_collapsed = false,
      output = '',
      output_collapsed = false,
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
      el.input = el.input .. content
      add_updated_content(diff, el, content)
    end
    -- missing -> drop silently

  elseif variant == 'SubAgentStart' then
    merge_diff(diff, collapse_open_thinking(model))
    local el = find_pending_subagent(model, data.tool_call_id)
    if el then
      el.input_open = false
      if visual_lines(el.input) > 2 then
        el.input_collapsed = true
      end
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
        input_collapsed = false,
        output = '',
        output_collapsed = false,
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
      if visual_lines(el.input) > 2 then
        el.input_collapsed = true
      end
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
        input_collapsed = false,
        output = '',
        output_collapsed = false,
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
      -- Long streamed output auto-collapses to a preview.
      if visual_lines(last.output) > 2 then
        last.output_collapsed = true
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
-- render(model, diff, ctx) is the single entry point; ctx = { buf, ns, bulk }.

-- Renderer-owned bookkeeping keyed per model (weak keys: a discarded model
-- releases its state). heights = element region height in buffer rows;
-- mat_len = materialized content length (chars) for streaming thinking blocks;
-- nav/thinking = per-element extmark id maps for the navigation ranges and the
-- thinking indicator/expand-hint marks, with *_ids as the reverse (id -> el_id)
-- indexes so later phases can map a cursor line back to the element;
-- hl = per-element list of { ns, id } highlight extmarks owned by the rebuilt
-- element kinds (thinking_block / tool_call / subagent) so a rebuild can delete
-- them (see del_hl_marks); hl_upto = last highlighted row per element so
-- streaming never re-highlights an already-highlighted row (see
-- render_updated_content).
local renderer_state = setmetatable({}, { __mode = 'k' })

local function get_renderer_state(model)
  local st = renderer_state[model]
  if not st then
    st = { heights = {}, mat_len = {}, nav = {}, nav_ids = {}, thinking = {}, thinking_ids = {}, thinking_kinds = {}, labels = {}, hl = {}, hl_upto = {} }
    renderer_state[model] = st
  end
  return st
end

-- Split text on '\n' into buffer rows. lines('') = { '' } (one empty row):
-- the streaming blank that content chunks consume.
local function lines(text)
  return vim.split(text or '', '\n', { plain = true })
end

-- Resolve an element's start-row anchor extmark to its current 0-indexed row,
-- or nil when the mark is gone (defensive: the update is skipped).
local function anchor_row(buf, el)
  if not el.anchor then return nil end
  local pos = vim.api.nvim_buf_get_extmark_by_id(buf, gen_ns, el.anchor, {})
  if pos and pos[1] then return pos[1] end
  return nil
end

-- Set a label row's virt_text overlay (the label overlay convention: prefix +
-- optional timestamp) without touching the buffer's real rows. Reuses the
-- element's existing overlay mark id (reuse_id) so repeated status updates
-- move one mark instead of stacking new ones on the row.
local function set_label_overlay(buf, ns, row, virt, reuse_id)
  local id = vim.api.nvim_buf_set_extmark(buf, ns, row, 0, {
    id = reuse_id,
    virt_text = virt,
    virt_text_pos = 'overlay',
  })
  return id
end

-- Plain label virt text: prefix + '  HH:MM:SS' timestamp.
local function label_virt(prefix, hl_group, created_at)
  local virt = { { prefix, hl_group } }
  local ts = format_time(created_at)
  if ts then table.insert(virt, { '  ' .. ts, 'TCodeTokens' }) end
  return virt
end

-- Collapse embedded newlines in wire-derived overlay text: virt_text rows
-- must stay on one line, so any '\n' in a status / name / description is
-- replaced with a space (nil-safe).
local function single_line(s)
  return tostring(s or ''):gsub('\n', ' ')
end

-- The width-dependent truncated args/input preview: flat text cut to width*2
-- chars; the width comes from the display window, defaulting to 80.
local function compute_preview(text, buf)
  local win = vim.fn.bufwinid(buf)
  local width = (win ~= -1) and vim.api.nvim_win_get_width(win) or 80
  local flat = text:gsub('\n', '\\n')
  return flat:sub(1, width * 2), width
end

-- Hidden visual line count for the '[... press o to expand N more lines]' hint.
local function hidden_visual_lines(buf, text, preview, width)
  local win = vim.fn.bufwinid(buf)
  local w = (win ~= -1) and vim.api.nvim_win_get_width(win) or 80
  local visual_count = 0
  for _, line in ipairs(lines(text)) do
    visual_count = visual_count + math.max(1, math.ceil(#line / w))
  end
  local kept_visual = math.max(1, math.ceil(#preview / width))
  return visual_count - kept_visual
end

-- Re-derive (create or update) a navigation range extmark for an element.
-- end_row is EXCLUSIVE. The mark id is indexed per element in the renderer
-- state so later phases can map a cursor line back to the element. Nav marks
-- carry no virt text. The reverse index is keyed by NAMESPACE because extmark
-- ids are namespace-local: um_ns / tc_ns / sa_ns each allocate id 1, so a
-- bare id cannot identify a mark across namespaces.
local function set_nav_extmark(state, buf, nav_ns, el, start_row, end_row)
  local nav_id = state.nav[el.id]
  if nav_id then
    local pos = vim.api.nvim_buf_get_extmark_by_id(buf, nav_ns, nav_id, {})
    if pos and pos[1] then
      -- Move the mark to the region start: region rebuilds shift the mark
      -- inside the replaced range, so the stored position is stale.
      vim.api.nvim_buf_set_extmark(buf, nav_ns, start_row, 0, {
        id = nav_id, end_row = end_row, end_col = 0,
      })
      return
    end
    -- The mark was deleted by a region rebuild; drop the stale index entry.
    state.nav[el.id] = nil
    state.nav_ids[nav_ns][nav_id] = nil
  end
  local id = vim.api.nvim_buf_set_extmark(buf, nav_ns, start_row, 0, {
    end_row = end_row, end_col = 0,
  })
  state.nav[el.id] = id
  if not state.nav_ids[nav_ns] then state.nav_ids[nav_ns] = {} end
  state.nav_ids[nav_ns][id] = el.id
end

-- Delete the thinking_ns indicator / preview-hint extmarks belonging to an
-- element (via the per-element index) before a rebuild replaces its region.
-- An element may own several marks (e.g. an args preview and an output
-- preview), so the index holds a list per element. The id -> element maps
-- ARE cleared here: preview/collapse hint marks are re-created with the SAME
-- ids after the rebuild (see set_preview_hint_mark / set_collapse_hint_mark),
-- so a captured mark id keeps resolving through element_for_mark while the
-- maps stay free of entries for marks that no longer exist.
local function del_thinking_marks(state, buf, el_id)
  local list = state.thinking[el_id]
  if list then
    for _, mark_id in ipairs(list) do
      pcall(vim.api.nvim_buf_del_extmark, buf, thinking_ns, mark_id)
      state.thinking_ids[mark_id] = nil
      state.thinking_kinds[mark_id] = nil
    end
    state.thinking[el_id] = nil
  end
end

local function add_thinking_mark(state, el_id, mark_id, kind)
  local list = state.thinking[el_id]
  if not list then list = {}; state.thinking[el_id] = list end
  list[#list + 1] = mark_id
  state.thinking_ids[mark_id] = el_id
  state.thinking_kinds[mark_id] = kind
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

-- Collapsed thinking indicator: virt overlay at the anchor row. Reuses the
-- element's existing mark id (reuse_id) so toggles holding a captured id
-- keep resolving after a collapse/expand cycle.
local function set_thinking_collapsed_mark(state, buf, el, mark_row, reuse_id)
  local id = vim.api.nvim_buf_set_extmark(buf, thinking_ns, mark_row, 0, {
    id = reuse_id,
    virt_text = { { '[Thinking... press o to expand]', 'TCodeTokens' } },
    virt_text_pos = 'overlay',
  })
  state.thinking[el.id] = { id }
  state.thinking_ids[id] = el.id
  state.thinking_kinds[id] = 'thinking'
end

-- Expanded thinking: range mark with a collapse hint line above it.
local function set_thinking_expanded_mark(state, buf, el, mark_row, content_row_count, reuse_id)
  local id = vim.api.nvim_buf_set_extmark(buf, thinking_ns, mark_row, 0, {
    id = reuse_id,
    end_row = mark_row + content_row_count,
    end_col = 0,
    virt_lines = { { { '[Thinking... press o to collapse]', 'TCodeTokens' } } },
    virt_lines_above = true,
  })
  state.thinking[el.id] = { id }
  state.thinking_ids[id] = el.id
  state.thinking_kinds[id] = 'thinking'
end

-- Collapsed args/input/output preview hint: virt line below the preview row.
-- reuse_id (optional) keeps the element's existing mark id across rebuilds so
-- a captured id stays valid (the mark is recreated with the same id).
local function set_preview_hint_mark(state, buf, el, preview_row, hidden_visual, kind, reuse_id)
  local id = vim.api.nvim_buf_set_extmark(buf, thinking_ns, preview_row, 0, {
    id = reuse_id,
    virt_lines = { { { '[... press o to expand ' .. hidden_visual .. ' more lines]', 'TCodeTokens' } } },
  })
  add_thinking_mark(state, el.id, id, kind)
end

-- Expanded args/input/output content hint: virt line above the content span;
-- `o` anywhere in the content (end_row covers it) collapses it again.
-- reuse_id (optional) keeps the element's existing mark id across rebuilds.
local function set_collapse_hint_mark(state, buf, el, mark_row, content_row_count, kind, reuse_id)
  local id = vim.api.nvim_buf_set_extmark(buf, thinking_ns, mark_row, 0, {
    id = reuse_id,
    end_row = mark_row + content_row_count,
    end_col = 0,
    virt_lines = { { { '[... press o to collapse]', 'TCodeTokens' } } },
    virt_lines_above = true,
  })
  add_thinking_mark(state, el.id, id, kind)
end

-- Tool-call label status overlay (status + timestamp + cancel hint).
local TC_STATUS = {
  generating = { text = 'generating', hl = 'TCodeTool', cancel = true },
  running = { text = 'running', hl = 'TCodeTool', cancel = true },
  permission = { text = 'permission', hl = 'TCodePermission' },
  done = { text = 'done', hl = 'TCodeSuccess' },
  failed = { text = 'failed', hl = 'TCodeError' },
  cancelled = { text = 'cancelled', hl = 'TCodeError' },
  denied = { text = 'denied', hl = 'TCodeError' },
}

local function tool_label_virt(el)
  local s = TC_STATUS[el.status] or { text = 'done', hl = 'TCodeSuccess' }
  local virt = {
    { '>>> TOOL: ', 'TCodeTool' },
    { '[' .. s.text .. ']', s.hl },
    -- tool_name is wire-derived and rendered as overlay virt_text, which must
    -- stay on one line: collapse any embedded newlines defensively (same
    -- pattern as the subagent description).
    { ' ' .. single_line(el.tool_name), 'TCodeTool' },
  }
  local ts = format_time(el.created_at)
  if ts then table.insert(virt, { '  ' .. ts, 'TCodeTokens' }) end
  if s.cancel then table.insert(virt, { '  [Ctrl-k to cancel]', 'TCodeTokens' }) end
  return virt
end

-- Subagent label overlay: status + optional timestamp/tokens + description.
local function subagent_label_virt(el)
  local status_text, status_hl
  if el.status == 'generating' then
    status_text, status_hl = 'generating', 'TCodeTool'
  elseif el.status == 'running' then
    status_text, status_hl = 'running', 'TCodeTool'
  elseif el.status == 'continuing' then
    status_text, status_hl = 'continuing', 'TCodeTool'
  elseif el.status == 'permission' then
    status_text, status_hl = 'permission', 'TCodePermission'
  elseif el.status == 'turn ended' then
    status_text, status_hl = 'turn ended', 'TCodeTokens'
  elseif el.status == 'done' then
    status_text, status_hl = 'done', 'TCodeSuccess'
  else
    -- Unknown statuses are wire-derived and can contain '\n': collapse any
    -- embedded newlines so the overlay stays on one line.
    status_text, status_hl = single_line(el.status or 'done'), 'TCodeError'
  end
  local virt = {
    { '>>> SUB-AGENT: ', 'TCodeTool' },
    { '[' .. status_text .. ']', status_hl },
  }
  local ts = format_time(el.created_at)
  if ts then table.insert(virt, { '  ' .. ts, 'TCodeTokens' }) end
  if el.input_tokens and el.output_tokens then
    table.insert(virt, {
      string.format('  [%d in / %d out]', el.input_tokens, el.output_tokens),
      'TCodeTokens',
    })
  end
  -- Descriptions are rendered as overlay virt_text, which must stay on one
  -- line: collapse any embedded newlines defensively.
  local desc = single_line(el.description)
  table.insert(virt, { ' ' .. desc, 'TCodeTool' })
  return virt
end

local function system_message_hl(level)
  if level == 'Warning' then return 'TCodeSystemWarning' end
  if level == 'Error' then return 'TCodeSystemError' end
  return 'TCodeSystemInfo'
end

-- Virtual-text parts for an end_info row (tokens + status). Mirrors the
-- old token/status line semantics exactly.
local function end_info_virt_parts(el)
  local virt_parts = {}
  local tokens = el.tokens or {}
  local token_prefix = el.token_prefix
  if tokens.input_tokens and tokens.output_tokens then
    local has_tokens = not token_prefix or (tokens.input_tokens > 0 or tokens.output_tokens > 0)
    if has_tokens then
      local cache_read = tokens.cache_read_input_tokens or 0
      local processed_input = tokens.input_tokens + (tokens.cache_creation_input_tokens or 0)
      local text
      if cache_read > 0 then
        local fmt = token_prefix
          and string.format('[%s: %%d in / %%d cache read / %%d out tokens]', token_prefix)
          or '[%d in / %d cache read / %d out tokens]'
        text = string.format(fmt, processed_input, cache_read, tokens.output_tokens)
      else
        local fmt = token_prefix
          and string.format('[%s: %%d in / %%d out tokens]', token_prefix)
          or '[%d in / %d out tokens]'
        text = string.format(fmt, processed_input, tokens.output_tokens)
      end
      table.insert(virt_parts, { text, 'TCodeTokens' })
    end
  end
  if el.end_status and el.end_status ~= 'Succeeded' then
    local prefix = token_prefix and ' [' .. string.upper(token_prefix) .. ' ' or ' ['
    table.insert(virt_parts, { prefix .. single_line(el.end_status) .. ']', 'TCodeError' })
  end
  return virt_parts
end

-- '► END' token-total overlay text (mirrors the AssistantRequestEnd handler).
local function end_marker_text(el)
  local tokens = el.tokens or {}
  local total_cache_read = tokens.total_cache_read_tokens or 0
  local total_processed = (tokens.total_input_tokens or 0) + (tokens.total_cache_creation_tokens or 0)
  local total_output = tokens.total_output_tokens or 0
  if total_cache_read > 0 then
    return string.format('[Total: %d in / %d cache read / %d out tokens]',
      total_processed, total_cache_read, total_output)
  end
  return string.format('[Total: %d in / %d out tokens]', total_processed, total_output)
end

-- Pure projection: the element's buffer rows derived ENTIRELY from model state.
-- Rendering the same state twice yields the same rows.
local function render_element(el, ctx)
  if el.type == 'user_message' then
    local out = { '► USER' }
    for _, l in ipairs(lines(el.content)) do out[#out + 1] = l end
    return out
  elseif el.type == 'assistant_message' then
    local out = { '► ASSISTANT' }
    local content = el.content or ''
    if content == '' then
      out[#out + 1] = ''
    else
      for _, l in ipairs(lines(content)) do out[#out + 1] = l end
    end
    return out
  elseif el.type == 'thinking_block' then
    return lines(el.content)
  elseif el.type == 'tool_call' then
    local out = { '► TOOL', TC_FENCE }
    local args = el.args or ''
    if el.args_collapsed then
      out[#out + 1] = compute_preview(args, ctx.buf)
    else
      for _, l in ipairs(lines(args)) do out[#out + 1] = l end
    end
    if not el.args_open then out[#out + 1] = TC_FENCE end
    if el.output_started then
      out[#out + 1] = TC_FENCE
      local output = el.output or ''
      if el.output_collapsed then
        out[#out + 1] = compute_preview(output, ctx.buf)
      elseif output == '' then
        out[#out + 1] = ''
      else
        for _, l in ipairs(lines(output)) do out[#out + 1] = l end
      end
      if not el.output_open then out[#out + 1] = TC_FENCE end
    end
    return out
  elseif el.type == 'subagent' then
    local out = { '► SUBAGENT', TC_FENCE }
    local input = el.input or ''
    if el.input_collapsed then
      out[#out + 1] = compute_preview(input, ctx.buf)
    else
      for _, l in ipairs(lines(input)) do out[#out + 1] = l end
    end
    if not el.input_open then
      out[#out + 1] = TC_FENCE
      local output = el.output or ''
      if el.output_collapsed then
        out[#out + 1] = compute_preview(output, ctx.buf)
      elseif output == '' then
        out[#out + 1] = ''
      else
        for _, l in ipairs(lines(output)) do out[#out + 1] = l end
      end
      if el.error then
        out[#out + 1] = ''
        for _, l in ipairs(lines('Error: ' .. el.error)) do out[#out + 1] = l end
      end
    end
    return out
  elseif el.type == 'system_message' then
    local out = { '► SYSTEM' }
    for _, l in ipairs(lines(el.message)) do out[#out + 1] = l end
    return out
  elseif el.type == 'media' then
    if not M.display_file then return nil end
    local session_dir = vim.fn.fnamemodify(M.display_file, ':h')
    local abs_path = session_dir .. '/media/' .. el.relative_path
    return { '', '![img](file://' .. vim.uri_encode(abs_path) .. ')' }
  elseif el.type == 'retry' then
    -- The reason may be a multi-line message (e.g. a JSON error body); split
    -- it so no buffer row carries an embedded newline.
    local reason_lines = lines(el.reason or '')
    local out = { string.format('[Retrying... (attempt %d/%d) -- %s]', el.attempt, el.max_retries, reason_lines[1]) }
    for i = 2, #reason_lines do out[#out + 1] = reason_lines[i] end
    return out
  elseif el.type == 'end_info' then
    local has_error = type(el.error) == 'string' and el.error ~= ''
    if #end_info_virt_parts(el) == 0 and not has_error then
      return {} -- nothing to display: the row is skipped entirely
    end
    local out = { '► INFO' }
    if has_error then
      for _, l in ipairs(lines('Error: ' .. el.error)) do out[#out + 1] = l end
    end
    return out
  elseif el.type == 'end_marker' then
    return { '► END' }
  end
  return {}
end

-- Highlight the args rows (or the single preview row) of a tool call region.
local function apply_args_highlight(state, buf, ns, el, start_row)
  local args_rows = el.args_collapsed and 1 or #lines(el.args or '')
  for i = 0, args_rows - 1 do
    add_tracked_highlight(state, el, buf, ns, 'TCodeToolArgs', start_row + 2 + i)
  end
end

-- Highlight the input rows (or the single preview row) of a subagent region.
local function apply_input_highlight(state, buf, ns, el, start_row)
  local input_rows = el.input_collapsed and 1 or #lines(el.input or '')
  for i = 0, input_rows - 1 do
    add_tracked_highlight(state, el, buf, ns, 'TCodeToolArgs', start_row + 2 + i)
  end
end

-- Resolve the existing hint-mark ids for a tool/subagent element by KIND:
-- the args/input hint id and the output hint id. Scanning the per-element
-- mark list via the kinds map (not list position) keeps the ids stable when
-- only one hint exists — e.g. an args-less tool streams only output, so the
-- output mark must always come back as the SECOND return, never as the
-- (unused) args slot, or every rebuild would hand the output a fresh id.
-- Call BEFORE del_thinking_marks: that clears the id -> element/kind maps.
local function hint_reuse_ids(state, el)
  local args_input_id, output_id
  local list = state.thinking[el.id]
  if list then
    for _, mark_id in ipairs(list) do
      local kind = state.thinking_kinds[mark_id]
      if kind == 'args' or kind == 'input' then
        args_input_id = mark_id
      elseif kind == 'output' then
        output_id = mark_id
      end
    end
  end
  return args_input_id, output_id
end

-- Place the `o` preview/collapse hint marks of a tool-call region. Collapsed
-- args/output get an 'expand N more lines' hint on the preview row; expanded
-- non-empty content gets a 'press o to collapse' hint spanning its rows.
-- args_reuse_id / output_reuse_id keep the element's existing mark ids across
-- rebuilds; hint reuse ids are matched by kind ('args' vs 'output').
local function apply_tool_hints(state, buf, el, arow, args_reuse_id, output_reuse_id)
  local args_rows = el.args_collapsed and 1 or #lines(el.args or '')
  if el.args_collapsed then
    local preview, width = compute_preview(el.args, buf)
    set_preview_hint_mark(state, buf, el, arow + 2, hidden_visual_lines(buf, el.args, preview, width), 'args', args_reuse_id)
  elseif not el.args_open and el.args and el.args ~= '' then
    set_collapse_hint_mark(state, buf, el, arow + 2, args_rows, 'args', args_reuse_id)
  end
  if el.output_started and not el.output_open then
    -- args_open is always false once output_started is set (both ToolMessageStart
    -- paths close the args fence together with opening the output).
    local out_fence_row = arow + 2 + args_rows + 1
    local output = el.output or ''
    local output_rows = el.output_collapsed and 1 or #lines(output)
    local output_first_row = out_fence_row + 1
    if el.output_collapsed then
      local preview, width = compute_preview(output, buf)
      set_preview_hint_mark(state, buf, el, output_first_row, hidden_visual_lines(buf, output, preview, width), 'output', output_reuse_id)
    elseif output ~= '' then
      set_collapse_hint_mark(state, buf, el, output_first_row, output_rows, 'output', output_reuse_id)
    end
  end
end

-- Same for a subagent region (input + output).
local function apply_subagent_hints(state, buf, el, arow, input_reuse_id, output_reuse_id)
  local input_rows = el.input_collapsed and 1 or #lines(el.input or '')
  if el.input_collapsed then
    local preview, width = compute_preview(el.input, buf)
    set_preview_hint_mark(state, buf, el, arow + 2, hidden_visual_lines(buf, el.input, preview, width), 'input', input_reuse_id)
  elseif not el.input_open and el.input and el.input ~= '' then
    set_collapse_hint_mark(state, buf, el, arow + 2, input_rows, 'input', input_reuse_id)
  end
  if not el.input_open then
    -- input_open is false here (this very condition), so the close-fence
    -- offset is always one row.
    local output_first_row = arow + 2 + input_rows + 1
    local output = el.output or ''
    local output_rows = el.output_collapsed and 1 or #lines(output)
    if el.output_collapsed then
      local preview, width = compute_preview(output, buf)
      set_preview_hint_mark(state, buf, el, output_first_row, hidden_visual_lines(buf, output, preview, width), 'output', output_reuse_id)
    elseif output ~= '' then
      set_collapse_hint_mark(state, buf, el, output_first_row, output_rows, 'output', output_reuse_id)
    end
  end
end

-- Apply one `added` entry: render the element's region at the buffer tail
-- (replacing the initial un-deletable empty row on first_event), place the
-- start anchor and navigation extmarks, and apply per-row highlights.
local function render_added(model, el, state, ctx)
  local buf, ns, bulk = ctx.buf, ctx.ns, ctx.bulk

  if el.type == 'thinking_block' then
    -- Anchor = the pre-append buffer tail row: the row its content streams
    -- onto (append_text consumes it). Bulk defers the write entirely.
    local start_row = vim.api.nvim_buf_line_count(buf) - 1
    first_event = false
    if not bulk then
      append_text(buf, el.content)
    end
    -- Default (left) gravity: when a bulk block starts streaming live, the
    -- append lands exactly at the anchor row and must not push the anchor
    -- down — it is the region START, so it tracks the first content row.
    el.anchor = vim.api.nvim_buf_set_extmark(buf, gen_ns, start_row, 0, {})
    state.heights[el.id] = bulk and 1 or count_lines(el.content)
    state.mat_len[el.id] = bulk and 0 or #el.content
    if not bulk then
      for i = 0, state.heights[el.id] - 1 do
        add_tracked_highlight(state, el, buf, thinking_ns, 'TCodeThinking', start_row + i)
      end
      state.hl_upto[el.id] = start_row + state.heights[el.id] - 1
    end
    return
  end

  local el_lines = render_element(el, ctx)
  if not el_lines or #el_lines == 0 then return end -- e.g. an empty end_info

  local start_row
  if first_event and vim.api.nvim_buf_line_count(buf) == 1 then
    first_event = false
    vim.api.nvim_buf_set_lines(buf, 0, 1, false, el_lines)
    start_row = 0
  else
    first_event = false
    start_row = vim.api.nvim_buf_line_count(buf)
    append_lines(buf, el_lines)
  end

  el.anchor = vim.api.nvim_buf_set_extmark(buf, gen_ns, start_row, 0, { right_gravity = true })
  state.heights[el.id] = #el_lines

  if el.type == 'user_message' then
    set_label_overlay(buf, ns, start_row, label_virt('>>> USER', 'TCodeUser', el.created_at))
    set_nav_extmark(state, buf, um_ns, el, start_row, start_row + #el_lines)
  elseif el.type == 'assistant_message' then
    set_label_overlay(buf, ns, start_row, label_virt('>>> ASSISTANT', 'TCodeAssistant', el.created_at))
  elseif el.type == 'tool_call' then
    state.labels[el.id] = set_label_overlay(buf, ns, start_row, tool_label_virt(el))
    apply_args_highlight(state, buf, ns, el, start_row)
    set_nav_extmark(state, buf, tc_ns, el, start_row, start_row + #el_lines)
    -- A freshly added element has no pre-existing hint marks: no reuse ids.
    apply_tool_hints(state, buf, el, start_row, nil, nil)
    state.hl_upto[el.id] = start_row + #el_lines - 1
  elseif el.type == 'subagent' then
    state.labels[el.id] = set_label_overlay(buf, ns, start_row, subagent_label_virt(el))
    apply_input_highlight(state, buf, ns, el, start_row)
    set_nav_extmark(state, buf, sa_ns, el, start_row, start_row + #el_lines)
    -- A freshly added element has no pre-existing hint marks: no reuse ids.
    apply_subagent_hints(state, buf, el, start_row, nil, nil)
    state.hl_upto[el.id] = start_row + #el_lines - 1
  elseif el.type == 'system_message' then
    local hl = system_message_hl(el.level)
    set_label_overlay(buf, ns, start_row, { { '[' .. (el.level or 'Info'):upper() .. '] ', hl } })
    for i = 1, #el_lines - 1 do
      vim.api.nvim_buf_add_highlight(buf, ns, hl, start_row + i, 0, -1)
    end
  elseif el.type == 'retry' then
    for i = 0, #el_lines - 1 do
      vim.api.nvim_buf_add_highlight(buf, ns, 'TCodeTokens', start_row + i, 0, -1)
    end
  elseif el.type == 'end_info' then
    local virt_parts = end_info_virt_parts(el)
    local has_error = type(el.error) == 'string' and el.error ~= ''
    set_label_overlay(buf, ns, start_row,
      #virt_parts > 0 and virt_parts or { { '► ERROR', 'TCodeError' } })
    if has_error then
      for i = 1, #el_lines - 1 do
        vim.api.nvim_buf_add_highlight(buf, ns, 'TCodeError', start_row + i, 0, -1)
      end
    end
  elseif el.type == 'end_marker' then
    set_label_overlay(buf, ns, start_row, { { end_marker_text(el), 'TCodeTokens' } })
  end
end

-- Apply one `updated_all` entry for a thinking block. Collapse -> indicator
-- rows; expand -> full content + collapse hint; open -> the merge reopen,
-- which renders ONLY the un-materialized content tail (see mat_len below).
local function render_thinking_update(el, state, ctx, arow)
  local buf = ctx.buf
  local old_height = state.heights[el.id] or 0
  -- Preserve the element's mark id across state flips so captured ids (e.g.
  -- a test's indicator mark) keep resolving after a toggle.
  local existing = state.thinking[el.id]
  local reuse_id = existing and existing[1]
  del_thinking_marks(state, buf, el.id)
  del_hl_marks(state, buf, el.id)
  if el.state == 'collapsed' then
    vim.api.nvim_buf_set_lines(buf, arow, arow + old_height, false, { '', '', '' })
    state.heights[el.id] = 3
    state.mat_len[el.id] = #el.content
    set_thinking_collapsed_mark(state, buf, el, arow, reuse_id)
    state.hl_upto[el.id] = arow + 2
  elseif el.state == 'expanded' then
    local content_lines = lines(el.content)
    vim.api.nvim_buf_set_lines(buf, arow, arow + old_height, false, content_lines)
    state.heights[el.id] = #content_lines
    state.mat_len[el.id] = #el.content
    -- Place the range mark BEFORE the highlight loop so the reuse id is
    -- claimed deterministically (the highlight loop allocates its own ids).
    set_thinking_expanded_mark(state, buf, el, arow, #content_lines, reuse_id)
    for i = 0, #content_lines - 1 do
      add_tracked_highlight(state, el, buf, thinking_ns, 'TCodeThinking', arow + i)
    end
    state.hl_upto[el.id] = arow + #content_lines - 1
  else
    -- 'open' via merge: the region's rows hold only the collapse indicator
    -- (the streamed content was replaced at the collapse), so re-rendering the
    -- full content would make the old run reappear. Render the tail after
    -- mat_len — exactly the content that was never visible in the buffer.
    local tail = el.content:sub((state.mat_len[el.id] or 0) + 1)
    local content_lines = lines(tail)
    vim.api.nvim_buf_set_lines(buf, arow, arow + old_height, false, content_lines)
    state.heights[el.id] = #content_lines
    state.mat_len[el.id] = #el.content
    for i = 0, #content_lines - 1 do
      add_tracked_highlight(state, el, buf, thinking_ns, 'TCodeThinking', arow + i)
    end
    state.hl_upto[el.id] = arow + #content_lines - 1
  end
end

-- Apply one `updated_all` entry for a region element (tool_call / subagent):
-- rebuild the region [anchor, anchor + height) from full model state — this is
-- the ONLY path that materializes bulk-deferred content in one shot.
local function render_region_update(el, state, ctx, arow)
  local buf, ns = ctx.buf, ctx.ns
  local old_height = state.heights[el.id] or 0
  -- Capture the element's existing hint-mark ids BEFORE deleting them so the
  -- rebuilt marks keep the same ids (captured ids stay valid across toggles).
  -- Kind-based resolution (hint_reuse_ids) must run before del_thinking_marks:
  -- that clears the id -> kind map the resolution reads.
  local args_reuse, output_reuse = hint_reuse_ids(state, el)
  del_thinking_marks(state, buf, el.id)
  del_hl_marks(state, buf, el.id)
  local el_lines = render_element(el, ctx)
  vim.api.nvim_buf_set_lines(buf, arow, arow + old_height, false, el_lines)
  state.heights[el.id] = #el_lines

  if el.type == 'tool_call' then
    state.labels[el.id] = set_label_overlay(buf, ns, arow, tool_label_virt(el), state.labels[el.id])
    apply_args_highlight(state, buf, ns, el, arow)
    set_nav_extmark(state, buf, tc_ns, el, arow, arow + #el_lines)
    apply_tool_hints(state, buf, el, arow, args_reuse, output_reuse)
  else -- subagent
    state.labels[el.id] = set_label_overlay(buf, ns, arow, subagent_label_virt(el), state.labels[el.id])
    apply_input_highlight(state, buf, ns, el, arow)
    set_nav_extmark(state, buf, sa_ns, el, arow, arow + #el_lines)
    apply_subagent_hints(state, buf, el, arow, args_reuse, output_reuse)
    if el.error then
      local err_lines = lines('Error: ' .. el.error)
      local err_start = arow + #el_lines - #err_lines
      for i = 0, #err_lines - 1 do
        add_tracked_highlight(state, el, buf, ns, 'TCodeError', err_start + i)
      end
    end
  end
  state.hl_upto[el.id] = arow + #el_lines - 1
end

local append_only_types = {
  user_message = true, assistant_message = true, system_message = true,
  media = true, retry = true, end_info = true, end_marker = true,
}

local function model_next_element(model, el)
  for i, e in ipairs(model.elements) do
    if e == el then return model.elements[i + 1] end
  end
  return nil
end

-- Highlight the rows a streaming chunk introduced for a region element kind
-- (thinking_block / tool_call / subagent). Only NEW rows are highlighted —
-- rows at or below hl_upto already carry a mark, and a chunk without a
-- trailing newline joins the last content row (append_row), so re-highlighting
-- that row would stack one duplicate mark per chunk.
--
-- One exception: when a chunk containing a newline is inserted into a
-- ZERO-LENGTH join row (join_col == 0), the join row's right-gravity mark
-- slides onto a later row — the insert lands at col 0, the mark's own
-- position — leaving the join row unhighlighted and a dead mark past it.
-- Delete every slid mark and re-highlight the join row plus all new rows so
-- each keeps exactly one mark.
local function highlight_appended_rows(state, el, buf, ns, group, append_row, new_rows, join_col)
  local upto = state.hl_upto[el.id] or -1
  local start = math.max(append_row, upto + 1)
  local last = append_row + new_rows - 1
  if upto >= append_row and join_col == 0 and new_rows > 1 then
    local list = state.hl[el.id]
    if list then
      for j = #list, 1, -1 do
        local entry = list[j]
        if entry[1] == ns then
          local pos = vim.api.nvim_buf_get_extmark_by_id(buf, ns, entry[2], {})
          if pos and pos[1] > append_row then
            pcall(vim.api.nvim_buf_del_extmark, buf, ns, entry[2])
            table.remove(list, j)
          end
        end
      end
    end
    start = append_row
    last = append_row + new_rows - 1
  end
  for i = start, last do
    add_tracked_highlight(state, el, buf, ns, group, i)
  end
  state.hl_upto[el.id] = append_row + new_rows - 1
end

-- Apply one `updated_content` entry ({element, text}): append the delta at the
-- element's append point — the buffer tail when the element is the tail or
-- append-only, otherwise the row just above the NEXT element's anchor (the
-- anchor rides the insert, so repeated appends land in order). Under bulk the
-- write is skipped entirely; the next updated_all materializes it.
local function render_updated_content(model, entry, state, ctx)
  local buf, ns, bulk = ctx.buf, ctx.ns, ctx.bulk
  local el, text = entry[1], entry[2]
  if bulk then return end

  -- A bulk-started thinking block's first live append lands exactly on its
  -- anchor row and would push the anchor down with the inserted lines; capture
  -- the region start so it can be pinned back after the write.
  local pre_anchor = (el.type == 'thinking_block') and anchor_row(buf, el)

  local append_row
  local insert_below = false
  if el == model.tail or append_only_types[el.type] then
    append_row = vim.api.nvim_buf_line_count(buf) - 1
  else
    local next_el = model_next_element(model, el)
    local next_row = next_el and anchor_row(buf, next_el)
    if next_row then
      append_row = next_row - 1
      insert_below = true
    else
      append_row = vim.api.nvim_buf_line_count(buf) - 1
    end
  end
  -- Pre-append length of the join row, captured BEFORE the write: the streaming
  -- highlight dedup needs it to detect a newline chunk landing on a zero-length
  -- row (see highlight_appended_rows).
  local join_col
  if el.type == 'thinking_block' or el.type == 'tool_call' or el.type == 'subagent' then
    join_col = #(vim.api.nvim_buf_get_lines(buf, append_row, append_row + 1, false)[1] or '')
  end
  if insert_below then
    insert_text_at(buf, append_row, text)
  else
    append_text(buf, text)
  end

  local new_rows = count_lines(text)
  if state.heights[el.id] then
    state.heights[el.id] = state.heights[el.id] + (new_rows - 1)
  end

  if el.type == 'thinking_block' then
    if pre_anchor then
      pcall(vim.api.nvim_buf_del_extmark, buf, gen_ns, el.anchor)
      el.anchor = vim.api.nvim_buf_set_extmark(buf, gen_ns, pre_anchor, 0, {})
    end
    state.mat_len[el.id] = (state.mat_len[el.id] or 0) + #text
    highlight_appended_rows(state, el, buf, thinking_ns, 'TCodeThinking', append_row, new_rows, join_col)
  elseif el.type == 'tool_call' then
    if el.args_open then
      highlight_appended_rows(state, el, buf, ns, 'TCodeToolArgs', append_row, new_rows, join_col)
    end
    local arow = anchor_row(buf, el)
    if arow then set_nav_extmark(state, buf, tc_ns, el, arow, arow + state.heights[el.id]) end
  elseif el.type == 'subagent' then
    if el.input_open then
      highlight_appended_rows(state, el, buf, ns, 'TCodeToolArgs', append_row, new_rows, join_col)
    end
    local arow = anchor_row(buf, el)
    if arow then set_nav_extmark(state, buf, sa_ns, el, arow, arow + state.heights[el.id]) end
  end
end

-- Apply ONE diff to the buffer: order updated_all -> updated_content -> added.
-- A single event's diff never has two kinds touching the same element, so the
-- order is exact. Callers are responsible for the modifiable window.
local function apply_diff(model, diff, ctx)
  if not diff then return end
  local buf = ctx.buf
  if not vim.api.nvim_buf_is_valid(buf) then return end
  local state = get_renderer_state(model)
  for _, el in ipairs(diff.updated_all) do
    local arow = anchor_row(buf, el)
    if arow then
      if el.type == 'thinking_block' then
        render_thinking_update(el, state, ctx, arow)
      elseif el.type == 'tool_call' or el.type == 'subagent' then
        render_region_update(el, state, ctx, arow)
      else
        arow = nil -- nothing rebuilt; no anchor re-pin
      end
      -- The region rebuild shifts the start anchor to the end of the
      -- replaced range (right_gravity); pin it back to the region start.
      if arow then
        if el.anchor then
          pcall(vim.api.nvim_buf_del_extmark, buf, gen_ns, el.anchor)
        end
        el.anchor = vim.api.nvim_buf_set_extmark(buf, gen_ns, arow, 0, { right_gravity = true })
      end
    end
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
-- render_batch owns those.
local function render(model, diff, ctx)
  if not diff then return end
  local buf = ctx.buf
  if not vim.api.nvim_buf_is_valid(buf) then return end
  with_modifiable(buf, function()
    apply_diff(model, diff, ctx)
  end)
end

-- Apply an ORDERED LIST of per-event diffs (one apply per event) in a single
-- modifiable window. Computes was_at_bottom BEFORE any writes; after the
-- window, if the cursor was at the bottom, moves it to the end of the last
-- line so the viewport follows the stream. Kicks force_render_markdown once
-- per batch. A failing diff stops the batch (reported, not raised).
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

  with_modifiable(buf, function()
    for _, diff in ipairs(diffs) do
      local ok, err = pcall(apply_diff, model, diff, ctx)
      if not ok then
        vim.api.nvim_err_writeln('render error: ' .. tostring(err))
        break
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

-- Resolve the element whose collapsed/expanded thinking or args/input preview
-- mark covers the given buffer row (0-indexed). The renderer registers these
-- marks with the element id in its thinking id map; marks are ordered, first
-- match wins.
local function find_marked_element_at(model, buf, row)
  local state = get_renderer_state(model)
  local marks = vim.api.nvim_buf_get_extmarks(buf, thinking_ns, 0, -1, { details = true })
  for _, mark in ipairs(marks) do
    local mark_id = mark[1]
    local el_id = state.thinking_ids[mark_id]
    if el_id then
      local start_row = mark[2]
      local details = mark[4]
      local end_row = details and details.end_row
      local matches = (end_row and start_row <= row and row < end_row)
        or (not end_row and start_row == row)
      if matches then
        return model.by_id[el_id], state.thinking_kinds[mark_id]
      end
    end
  end
  return nil, nil
end

-- Resolve the element whose region covers the given buffer row (0-indexed),
-- scanning the model from the END (the most recent element wins). Height comes
-- from the renderer's per-element region height; elements without a resolved
-- anchor are skipped.
local function element_at_row(model, buf, row)
  local state = get_renderer_state(model)
  for i = #model.elements, 1, -1 do
    local el = model.elements[i]
    local anchor = anchor_row(buf, el)
    if anchor then
      local height = state.heights[el.id] or 1
      if row >= anchor and row < anchor + height then
        return el
      end
    end
  end
  return nil
end

-- Compat helpers (test suites call these with the old signatures). Each one
-- resolves the target element and routes through the reducer + renderer.

-- Resolve a thinking_ns mark id (indicator or preview hint) to its element.
local function element_for_mark(model, mark_id)
  local state = get_renderer_state(model)
  local el_id = state.thinking_ids[mark_id]
  if el_id then return model.by_id[el_id] end
  return nil
end

local function collapse_thinking(buf, ns)
  local d = collapse_open_thinking(model)
  if #d.updated_all > 0 then
    render(model, d, { buf = buf, ns = ns, bulk = false })
  end
end

local function toggle_thinking(buf, mark_id)
  local el = element_for_mark(model, mark_id)
  if el and el.type == 'thinking_block' then
    local d = toggle_thinking_element(model, el)
    render(model, d, { buf = buf, ns = ns, bulk = false })
  end
end

local function toggle_tool_call_args(buf, mark_id)
  local el = element_for_mark(model, mark_id)
  if el and (el.type == 'tool_call' or el.type == 'subagent') then
    local d = toggle_tool_call_args_element(model, el)
    render(model, d, { buf = buf, ns = ns, bulk = false })
  end
end

-- Apply one display event through the model + renderer. Kept as a thin
-- compat wrapper (the reader and keymaps now call apply/render directly).
local function render_event(buf, ns, event, envelope_id, bulk)
  local diff = apply(model, event, envelope_id)
  render(model, diff, { buf = buf, ns = ns, bulk = bulk })
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

  -- One-shot settle timer used to flush content deferred during the initial
  -- bulk load (thinking blocks / open args fences that never hit a collapse
  -- point, e.g. a crashed session). It is re-armed on every content read, so
  -- it only fires after the file has been quiet for SETTLE_MS — a live
  -- session keeps streaming and never flushes prematurely, while a static
  -- file (idle/crashed) materializes its deferred content once.
  local flush_timer = nil

  local function cancel_flush_timer()
    if flush_timer then
      flush_timer:stop()
      flush_timer:close()
      flush_timer = nil
    end
  end

  local function flush_deferred()
    if not vim.api.nvim_buf_is_valid(buf) then return end
    -- Reducer-level "close open elements" (collapse open thinking, close open
    -- args/input fences), rendered by render_batch (its own modifiable window,
    -- scroll, and markdown kick). The updated_all rebuilds materialize any
    -- bulk-deferred content from model state in one shot.
    local ok, err = pcall(function()
      local diff = close_open_elements(model)
      render_batch(model, { diff }, { buf = buf, ns = ns, bulk = false })
    end)
    if not ok then
      vim.api.nvim_err_writeln('flush_deferred render error: ' .. tostring(err))
    end
  end

  local function arm_flush_timer()
    cancel_flush_timer()
    flush_timer = vim.uv.new_timer()
    flush_timer:start(500, 0, vim.schedule_wrap(flush_deferred))
  end

  -- Stop the timer when the buffer goes away (harmless if already fired).
  vim.api.nvim_create_autocmd({ 'BufDelete', 'BufWipeout' }, {
    buffer = buf,
    once = true,
    callback = cancel_flush_timer,
  })

  local function check()
    local file = io.open(filepath, 'r')
    if not file then return end
    file:seek('set', state.last_size)
    local new_content = file:read('*all')
    file:close()

    if not new_content or #new_content == 0 then return end
    state.last_size = state.last_size + #new_content
    -- Any new content pushes the settle-flush out: while the file keeps
    -- growing the stream is live and collapse points handle fences naturally.
    arm_flush_timer()

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

      -- Render the whole batch inside render_batch's single modifiable window
      -- (bulk defers per-chunk content writes; the settle flush materializes).
      render_batch(model, diffs, { buf = buf, ns = ns, bulk = state.is_initial_load })
      if state.is_initial_load then
        state.is_initial_load = false
        -- No immediate flush here: arm_flush_timer (called on every content
        -- read above) fires once the file has been quiet for 500 ms, so a
        -- live session mid-args/mid-thinking keeps its fence open and
        -- subsequent chunks stream normally, while a static file (idle or
        -- crashed) materializes deferred content exactly once.
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

  -- Reset the model and first_event flag for this display session.
  model = new_model()
  first_event = true

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

  -- Context-aware 'o' keybinding: toggle thinking / tool args / input /
  -- output previews, or open the subagent / tool-call detail view from the
  -- element's label line only.
  vim.keymap.set('n', 'o', function()
    local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed

    -- Step 1: a marked element under the cursor (thinking indicator, a
    -- collapsed preview hint, or expanded content) -> toggle it through the
    -- reducer. Expanded content carries its own collapse hint, so `o` there
    -- collapses instead of opening the detail view.
    local el, kind = find_marked_element_at(model, buf, cursor_line)
    if el then
      local d
      if kind == 'thinking' then
        d = toggle_thinking_element(model, el)
      elseif kind == 'args' or kind == 'input' then
        d = toggle_tool_call_args_element(model, el)
      elseif kind == 'output' then
        d = toggle_tool_output_element(model, el)
      end
      if d and #d.updated_all > 0 then
        render(model, d, { buf = buf, ns = ns, bulk = false })
      end
      return
    end

    -- Step 2: the detail view opens only from the element's LABEL row (the
    -- anchor row), not from anywhere in its content region.
    el = element_at_row(model, buf, cursor_line)
    if el and (el.type == 'subagent' or el.type == 'tool_call') then
      local arow = anchor_row(buf, el)
      if arow == cursor_line then
        if el.type == 'subagent' and el.conversation_id then
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
    end
    -- Otherwise nothing under the cursor to act on.
  end, { buffer = true, silent = true, desc = 'Toggle preview or open detail' })

  -- Cancel tool or subagent with confirmation popup (Ctrl-k)
  vim.keymap.set('n', '<C-k>', function()
    if not M.exe_path or not M.session_id then
      vim.notify('Session info not available', vim.log.levels.ERROR)
      return
    end

    local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed
    local el = element_at_row(model, buf, cursor_line)
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

  -- Branch the conversation at the user message under the cursor (gb)
  if not is_subagent then
    vim.keymap.set('n', 'gb', function()
      if not M.exe_path or not M.session_id then
        vim.notify('Session info not available', vim.log.levels.ERROR)
        return
      end
      local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed
      local el = element_at_row(model, buf, cursor_line)
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
  first_event = true

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
