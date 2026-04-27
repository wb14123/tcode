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

-- Ensure a buffer is modifiable before writing to it.
-- Returns false if the buffer is invalid, so caller can bail out.
-- Note: caller is responsible for resetting modifiable = false when done.
local function ensure_buf_modifiable(buf)
  if not vim.api.nvim_buf_is_valid(buf) then return false end
  vim.bo[buf].modifiable = true
  return true
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

-- Namespace and lookup table for tool-call range extmarks.
-- Maps extmark ID -> tool_call_id so we can find which tool call a cursor line belongs to.
local tc_ns = vim.api.nvim_create_namespace('tcode_tc_id')
local tc_extmark_ids = {}  -- extmark_id -> tool_call_id
local tc_tool_names = {}   -- tool_call_id -> tool_name
local tc_label_marks = {}  -- tool_call_id -> { extmark_id, ns, tool_name }

-- Namespace and lookup table for subagent range extmarks.
-- Maps extmark ID -> conversation_id so we can find which subagent a cursor line belongs to.
local sa_ns = vim.api.nvim_create_namespace('tcode_sa_id')
local sa_extmark_ids = {}  -- extmark_id -> conversation_id
local sa_label_marks = {}  -- conversation_id -> { extmark_id, ns, description }
local sa_input_marks = {}  -- tool_call_id -> { extmark_id, ns, tool_name }

-- Namespace for tool-call / subagent generation-state anchor extmarks.
-- These extmarks track where content for an in-progress tool call / subagent
-- lives in the buffer, so that mutations elsewhere (collapse, other tool
-- calls being inserted below, close-fence insertions) don't invalidate
-- position references. Rows are resolved from the extmarks at use time, not
-- stored as stale integers.
local gen_ns = vim.api.nvim_create_namespace('tcode_gen')

-- Flag to handle the initial empty line in Neovim buffers
local first_event = true

-- Thinking token state: track streaming thinking and collapsed entries
local thinking_ns = vim.api.nvim_create_namespace('tcode_thinking')
local thinking_entries = {}  -- extmark_id -> { content, expanded }
local thinking_state = {
  is_thinking = false,
  start_row = nil,
  content_parts = {},  -- accumulate chunks in a table, concat only when needed
  last_highlighted_row = nil,  -- track last highlighted row to avoid re-highlighting
}

-- Tool call / subagent argument generation state, keyed by tool_call_id
local tool_call_gen_state = {}
-- Each entry: {
--   args_open_mark_id  = <gen_ns extmark id>,  -- row of opening TC_FENCE line. Uses
--                                              -- default right_gravity=true so the
--                                              -- mark rides with the fence character
--                                              -- if another tool call's closing fence
--                                              -- is inserted at the same row from above.
--   args_close_mark_id = <gen_ns extmark id>,  -- row of closing TC_FENCE line, set by
--                                              -- close_args_fence. nil until then.
--   content_parts      = {},                   -- accumulated raw arg chunks.
--   last_highlighted_row = nil,                -- last buffer row highlighted during streaming.
--                                              -- Protocol invariant: no handler between
--                                              -- two ArgChunk events for the same id
--                                              -- mutates rows above the active fence, so
--                                              -- an absolute integer is safe here.
--   fence_closed       = false,                -- true once close_args_fence has run.
-- }
--
-- Row positions are ALWAYS resolved via the extmark ids at the moment of use,
-- not stored as stale integers, because edits for adjacent tool calls /
-- collapses / fence insertions can shift rows underneath us.

-- Maps tool_call_index -> tool_call_id (to look up state from ArgChunk events)
local tool_call_index_map = {}

-- For expand/collapse after generation is done, keyed by extmark id
local tool_call_gen_entries = {}
-- Each entry: { content = "full args text", expanded = false }

-- Tool output is wrapped in a long backtick-fenced code block to prevent
-- markdown/treesitter from interpreting partial HTML, XML, JSON, etc. as
-- markdown syntax. We use 10 backticks so tool output containing ``` won't
-- accidentally close the fence.
local TC_FENCE = '``````````'
local tc_fence_opened = {}  -- tool_call_id -> true once opening fence has been inserted

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

-- Render a label line with optional timestamp as virtual text
local function render_label(buf, ns, separator, prefix, hl_group, data)
  if first_event then
    first_event = false
    -- Neovim buffers start with one empty line that can't be deleted.
    -- Replace it with the first separator instead of appending.
    vim.api.nvim_buf_set_lines(buf, 0, 1, false, { separator })
  else
    append_lines(buf, { separator })
  end
  local label_line = vim.api.nvim_buf_line_count(buf) - 1
  local virt = { { prefix, hl_group } }
  local ts = format_time(data.created_at)
  if ts then
    table.insert(virt, { '  ' .. ts, 'TCodeTokens' })
  end
  local extmark_id = vim.api.nvim_buf_set_extmark(buf, ns, label_line, 0, {
    virt_text = virt,
    virt_text_pos = 'overlay',
  })
  return label_line, extmark_id
end

-- Update a tool call label extmark in-place with a status indicator.
local function update_tc_label(buf, tool_call_id, status_text, status_hl, show_cancel)
  local info = tc_label_marks[tool_call_id]
  if not info then return end
  local virt = {
    { '>>> TOOL: ', 'TCodeTool' },
    { '[' .. status_text .. ']', status_hl },
    { ' ' .. info.tool_name, 'TCodeTool' },
  }
  local ts = format_time(info.created_at)
  if ts then table.insert(virt, { '  ' .. ts, 'TCodeTokens' }) end
  if show_cancel then table.insert(virt, { '  [Ctrl-k to cancel]', 'TCodeTokens' }) end
  local mark_pos = vim.api.nvim_buf_get_extmark_by_id(buf, info.ns, info.extmark_id, {})
  if mark_pos and mark_pos[1] then
    vim.api.nvim_buf_set_extmark(buf, info.ns, mark_pos[1], mark_pos[2], {
      id = info.extmark_id, virt_text = virt, virt_text_pos = 'overlay',
    })
  end
end

--- Insert lines at a specific row (pushing existing content down).
--- Returns the row where insertion started.
local function insert_lines_at(buf, row, lines)
  vim.api.nvim_buf_set_lines(buf, row, row, false, lines)
  return row
end

--- Insert text at the end of a specific row, supporting multi-line text.
local function insert_text_at(buf, row, text)
  local cur_line = vim.api.nvim_buf_get_lines(buf, row, row + 1, false)[1] or ''
  local lines = vim.split(text, '\n', { plain = true })
  vim.api.nvim_buf_set_text(buf, row, #cur_line, row, #cur_line, lines)
end

--- Render a token/status info line as virtual text, but errors as real text.
--- If insert_at is provided, inserts at that row instead of appending at buffer end.
---
--- Token semantics (Anthropic API):
---   input_tokens: tokens NOT involved in any cache (not read from, not written to)
---   cache_creation_input_tokens: tokens fully processed AND written to a new cache (1.25x cost)
---   cache_read_input_tokens: tokens served from an existing cache (0.1x cost)
---
--- Display:
---   "in" = input_tokens + cache_creation_input_tokens (all tokens actually processed)
---   "cache read" = cache_read_input_tokens (tokens cheaply served from cache)
---   "out" = output_tokens
local function render_info(buf, ns, data, token_prefix, insert_at)
  -- Collect virtual text parts for tokens/status metadata FIRST,
  -- so we can skip writing the buffer line entirely when there's nothing to show.
  local virt_parts = {}
  if data.input_tokens and data.output_tokens then
    local has_tokens = not token_prefix or (data.input_tokens > 0 or data.output_tokens > 0)
    if has_tokens then
      local text
      -- "in" = input_tokens + cache_creation (all tokens actually processed by the model)
      -- "cache read" = cache_read_input_tokens (tokens served from cache, not reprocessed)
      local cache_read = data.cache_read_input_tokens or 0
      local processed_input = data.input_tokens + (data.cache_creation_input_tokens or 0)
      if cache_read > 0 then
        local fmt = token_prefix
          and string.format('[%s: %%d in / %%d cache read / %%d out tokens]', token_prefix)
          or '[%d in / %d cache read / %d out tokens]'
        text = string.format(fmt, processed_input, cache_read, data.output_tokens)
      else
        local fmt = token_prefix
          and string.format('[%s: %%d in / %%d out tokens]', token_prefix)
          or '[%d in / %d out tokens]'
        text = string.format(fmt, processed_input, data.output_tokens)
      end
      table.insert(virt_parts, { text, 'TCodeTokens' })
    end
  end
  if data.end_status and data.end_status ~= 'Succeeded' then
    local prefix = token_prefix and ' [' .. string.upper(token_prefix) .. ' ' or ' ['
    table.insert(virt_parts, { prefix .. data.end_status .. ']', 'TCodeError' })
  end

  local has_error = type(data.error) == 'string' and data.error ~= ''

  -- Nothing to display: skip writing the buffer line + extmark entirely
  if #virt_parts == 0 and not has_error then
    return nil
  end

  -- Write the separator line to the buffer
  local info_line
  if insert_at then
    insert_lines_at(buf, insert_at, { '► INFO' })
    info_line = insert_at
  else
    append_lines(buf, { '► INFO' })
    info_line = vim.api.nvim_buf_line_count(buf) - 1
  end

  -- Render tokens/status as virtual text overlay to conceal separator text
  vim.api.nvim_buf_set_extmark(buf, ns, info_line, 0, {
    virt_text = #virt_parts > 0 and virt_parts or { { '► ERROR', 'TCodeError' } },
    virt_text_pos = 'overlay',
  })

  -- Render error as real buffer text so it can wrap, be navigated, selected, copied
  if has_error then
    if insert_at then
      insert_lines_at(buf, info_line + 1, { '' })
      local error_start_line = info_line + 1
      local error_lines = vim.split('Error: ' .. data.error, '\n', { plain = true })
      vim.api.nvim_buf_set_lines(buf, error_start_line, error_start_line + 1, false, error_lines)
      for i = 0, #error_lines - 1 do
        vim.api.nvim_buf_add_highlight(buf, ns, 'TCodeError', error_start_line + i, 0, -1)
      end
    else
      append_lines(buf, { '' })
      local error_start_line = vim.api.nvim_buf_line_count(buf) - 1
      local error_lines = vim.split('Error: ' .. data.error, '\n', { plain = true })
      vim.api.nvim_buf_set_lines(buf, error_start_line, error_start_line + 1, false, error_lines)
      for i = 0, #error_lines - 1 do
        vim.api.nvim_buf_add_highlight(buf, ns, 'TCodeError', error_start_line + i, 0, -1)
      end
    end
  end

  return info_line
end

-- Find and update the range extmark for a tool_call_id to extend to end_row
local function extend_tc_extmark(buf, tool_call_id, end_row)
  local marks = vim.api.nvim_buf_get_extmarks(buf, tc_ns, 0, -1, {})
  for _, mark in ipairs(marks) do
    if tc_extmark_ids[mark[1]] == tool_call_id then
      vim.api.nvim_buf_set_extmark(buf, tc_ns, mark[2], mark[3], {
        id = mark[1],
        end_row = end_row,
        end_col = 0,
      })
      break
    end
  end
end

--- Get the end_row of a tool-call's range extmark. Returns nil if not found.
local function get_tc_extmark_end_row(buf, tool_call_id)
  local marks = vim.api.nvim_buf_get_extmarks(buf, tc_ns, 0, -1, { details = true })
  for _, mark in ipairs(marks) do
    if tc_extmark_ids[mark[1]] == tool_call_id then
      local details = mark[4]
      if details and details.end_row then
        return details.end_row
      end
    end
  end
  return nil
end

-- Find and update the range extmark for a conversation_id to extend to end_row
local function extend_sa_extmark(buf, conversation_id, end_row)
  local marks = vim.api.nvim_buf_get_extmarks(buf, sa_ns, 0, -1, {})
  for _, mark in ipairs(marks) do
    if sa_extmark_ids[mark[1]] == conversation_id then
      vim.api.nvim_buf_set_extmark(buf, sa_ns, mark[2], mark[3], {
        id = mark[1],
        end_row = end_row,
        end_col = 0,
      })
      break
    end
  end
end

--- Get the end_row of a subagent's range extmark. Returns nil if not found.
local function get_sa_extmark_end_row(buf, conversation_id)
  local marks = vim.api.nvim_buf_get_extmarks(buf, sa_ns, 0, -1, { details = true })
  for _, mark in ipairs(marks) do
    if sa_extmark_ids[mark[1]] == conversation_id then
      local details = mark[4]
      if details and details.end_row then
        return details.end_row
      end
    end
  end
  return nil
end

-- Collapse streaming thinking content into a single indicator line
local function collapse_thinking(buf, ns)
  if not thinking_state.is_thinking then return end

  local start_row = thinking_state.start_row
  local end_row = vim.api.nvim_buf_line_count(buf) - 1

  -- Replace thinking lines with indicator line + spacer + empty line for subsequent content
  if end_row >= start_row then
    vim.api.nvim_buf_set_lines(buf, start_row, end_row + 1, false, { '', '', '' })
  end

  -- Place indicator extmark
  local mark_id = vim.api.nvim_buf_set_extmark(buf, thinking_ns, start_row, 0, {
    virt_text = { { '[Thinking... press o to expand]', 'TCodeTokens' } },
    virt_text_pos = 'overlay',
  })

  -- Store for later expansion
  thinking_entries[mark_id] = {
    content = table.concat(thinking_state.content_parts),
    expanded = false,
  }

  -- Reset thinking state
  thinking_state.is_thinking = false
  thinking_state.content_parts = {}
  thinking_state.start_row = nil
  thinking_state.last_highlighted_row = nil
end

-- Find a thinking extmark at the given buffer line (0-indexed)
-- Only returns extmarks that are tracked in thinking_entries (not highlights)
local function find_thinking_at_line(buf, line)
  local marks = vim.api.nvim_buf_get_extmarks(buf, thinking_ns, 0, -1, { details = true })
  for _, mark in ipairs(marks) do
    local mark_id = mark[1]
    -- Only consider marks that are tracked thinking entries (not highlights)
    if thinking_entries[mark_id] then
      local start_row = mark[2]
      local details = mark[4]
      local end_row = details.end_row or start_row
      if line >= start_row and line <= end_row then
        return mark_id
      end
    end
  end
  return nil
end

-- Toggle thinking content expand/collapse inline
local function toggle_thinking(buf, mark_id)
  local entry = thinking_entries[mark_id]
  if not entry then return end

  vim.bo[buf].modifiable = true

  local mark = vim.api.nvim_buf_get_extmark_by_id(buf, thinking_ns, mark_id, {})
  local start_row = mark[1]
  local content_lines = vim.split(entry.content, '\n', { plain = true })

  if entry.expanded then
    -- Collapse: replace content lines with single blank indicator line
    vim.api.nvim_buf_set_lines(buf, start_row, start_row + #content_lines, false, { '' })
    vim.api.nvim_buf_set_extmark(buf, thinking_ns, start_row, 0, {
      id = mark_id,
      virt_text = { { '[Thinking... press o to expand]', 'TCodeTokens' } },
      virt_text_pos = 'overlay',
    })
    entry.expanded = false
  else
    -- Expand: replace blank indicator line with content
    vim.api.nvim_buf_set_lines(buf, start_row, start_row + 1, false, content_lines)
    -- Apply thinking highlight to all expanded lines
    for i = 0, #content_lines - 1 do
      vim.api.nvim_buf_add_highlight(buf, thinking_ns, 'TCodeThinking', start_row + i, 0, -1)
    end
    vim.api.nvim_buf_set_extmark(buf, thinking_ns, start_row, 0, {
      id = mark_id,
      end_row = start_row + #content_lines - 1,
      end_col = 0,
      virt_lines = { { { '[Thinking... press o to collapse]', 'TCodeTokens' } } },
      virt_lines_above = true,
    })
    entry.expanded = true
  end

  vim.bo[buf].modifiable = false
end

-- Collapse streaming tool call args into a short preview with expand hint
-- Count visual (displayed) lines for a set of buffer lines, accounting for wrap.
local function count_visual_lines(buf, lines)
  local win = vim.fn.bufwinid(buf)
  local width = (win ~= -1) and vim.api.nvim_win_get_width(win) or 80
  local total = 0
  for _, line in ipairs(lines) do
    total = total + math.max(1, math.ceil(#line / width))
  end
  return total
end

-- Resolve a gen_ns extmark id to its current 0-indexed buffer row, or nil if
-- the mark has been deleted or never existed.
local function get_gen_mark_row(buf, mark_id)
  if not mark_id then return nil end
  local pos = vim.api.nvim_buf_get_extmark_by_id(buf, gen_ns, mark_id, {})
  if pos and pos[1] then return pos[1] end
  return nil
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

local function collapse_tool_call_args(buf, tool_call_id)
  local state = tool_call_gen_state[tool_call_id]
  if not state then return end

  local full_content = table.concat(state.content_parts)
  local content_lines = vim.split(full_content, '\n', { plain = true })
  local line_count = #content_lines

  -- Decide based on visual (wrapped) line count, not raw buffer lines.
  local visual_count = count_visual_lines(buf, content_lines)
  if visual_count <= 2 then return end

  -- Resolve the current opening-fence row via extmark. It may have shifted
  -- since the mark was placed (other tool calls / fences being inserted).
  local args_open_row = get_gen_mark_row(buf, state.args_open_mark_id)
  if not args_open_row then return end  -- anchor lost; skip collapse

  -- The args content spans rows [args_open_row + 1, args_open_row + line_count].
  local args_start = args_open_row + 1
  local args_end = args_start + line_count - 1

  -- Compute how much text fits in ~2 visual rows
  local win = vim.fn.bufwinid(buf)
  local width = (win ~= -1) and vim.api.nvim_win_get_width(win) or 80
  local keep_chars = width * 2

  -- Build the truncated preview: take characters up to keep_chars, on a single line
  local flat = full_content:gsub('\n', '\\n')
  local preview = flat:sub(1, keep_chars)
  local kept_visual = math.max(1, math.ceil(#preview / width))
  local hidden_visual = visual_count - kept_visual

  -- Replace all content lines with the single truncated preview line.
  -- args_close_mark_id (if already set by close_args_fence) sits at the end
  -- boundary of the replaced range. With its default right_gravity, nvim
  -- treats it as "after the replace" and shifts it by (1 - line_count),
  -- keeping it pointed at the closing-fence row after collapse.
  vim.api.nvim_buf_set_lines(buf, args_start, args_end + 1, false, { preview })
  vim.api.nvim_buf_add_highlight(buf, -1, 'TCodeToolArgs', args_start, 0, -1)

  local mark_id = vim.api.nvim_buf_set_extmark(buf, thinking_ns, args_start, 0, {
    virt_lines = { { { '[... press o to expand ' .. hidden_visual .. ' more lines]', 'TCodeTokens' } } },
  })

  -- Store for expand/collapse toggle
  tool_call_gen_entries[mark_id] = {
    content = full_content,
    expanded = false,
  }
end

-- Close the args fenced code block for a tool call / subagent gen_state
-- entry. Idempotent: returns early if fence_closed is already true.
--
-- Uses args_open_mark_id to locate the correct row — NOT end of buffer — so
-- the closing fence lands inside this tool call's own region even when other
-- tool calls / content exist below it. After insertion, args_close_mark_id
-- is set so that subsequent ToolMessageStart / SubAgentStart handlers can
-- find the row via extmark.
local function close_args_fence(buf, tool_call_id)
  local state = tool_call_gen_state[tool_call_id]
  if not state or state.fence_closed then return end

  if not ensure_buf_modifiable(buf) then return end

  local args_open_row = get_gen_mark_row(buf, state.args_open_mark_id)
  local close_row
  if args_open_row == nil then
    -- Anchor lost (buffer invalidated or mark deleted). Fall back to append.
    append_lines(buf, { TC_FENCE })
    close_row = vim.api.nvim_buf_line_count(buf) - 1
  else
    -- Compute where the closing fence goes: immediately after the streamed
    -- args content. The content occupies `content_line_count` rows starting
    -- at args_open_row + 1 (the row just after the opening fence).
    local full_content = table.concat(state.content_parts)
    local content_line_count = #vim.split(full_content, '\n', { plain = true })
    close_row = args_open_row + 1 + math.max(content_line_count, 1)
    insert_lines_at(buf, close_row, { TC_FENCE })
  end

  state.args_close_mark_id = vim.api.nvim_buf_set_extmark(buf, gen_ns, close_row, 0, {})

  -- Collapse has to run AFTER setting args_close_mark_id so that the extmark
  -- can shift along with the replaced content region.
  collapse_tool_call_args(buf, tool_call_id)
  state.fence_closed = true
end

-- Find a tool call args extmark at the given buffer line (0-indexed)
local function find_tool_args_at_line(buf, line)
  local marks = vim.api.nvim_buf_get_extmarks(buf, thinking_ns, { line, 0 }, { line, -1 }, {})
  for _, mark in ipairs(marks) do
    if tool_call_gen_entries[mark[1]] then
      return mark[1]
    end
  end
  return nil
end

-- Toggle tool call args expand/collapse inline
local function toggle_tool_call_args(buf, mark_id)
  local entry = tool_call_gen_entries[mark_id]
  if not entry then return end

  vim.bo[buf].modifiable = true

  local pos = vim.api.nvim_buf_get_extmark_by_id(buf, thinking_ns, mark_id, {})
  if not pos or #pos == 0 then
    vim.bo[buf].modifiable = false
    return
  end
  local mark_row = pos[1]

  local content_lines = vim.split(entry.content, '\n', { plain = true })
  local visual_count = count_visual_lines(buf, content_lines)

  local win = vim.fn.bufwinid(buf)
  local width = (win ~= -1) and vim.api.nvim_win_get_width(win) or 80

  if entry.expanded then
    -- Collapse: replace full content lines with a single truncated preview line
    local first_line_row = mark_row - #content_lines + 1
    local keep_chars = width * 2
    local flat = entry.content:gsub('\n', '\\n')
    local preview = flat:sub(1, keep_chars)
    local kept_visual = math.max(1, math.ceil(#preview / width))
    local hidden_visual = visual_count - kept_visual

    vim.api.nvim_buf_set_lines(buf, first_line_row, first_line_row + #content_lines, false, { preview })
    vim.api.nvim_buf_add_highlight(buf, -1, 'TCodeToolArgs', first_line_row, 0, -1)

    vim.api.nvim_buf_set_extmark(buf, thinking_ns, first_line_row, 0, {
      id = mark_id,
      virt_lines = { { { '[... press o to expand ' .. hidden_visual .. ' more lines]', 'TCodeTokens' } } },
    })

    entry.expanded = false
  else
    -- Expand: replace single preview line with all original content lines
    -- The extmark is on mark_row (the single preview line when collapsed)
    local first_line_row = mark_row
    vim.api.nvim_buf_set_lines(buf, first_line_row, first_line_row + 1, false, content_lines)

    -- Highlight all lines
    for i = 0, #content_lines - 1 do
      vim.api.nvim_buf_add_highlight(buf, -1, 'TCodeToolArgs', first_line_row + i, 0, -1)
    end

    -- Update extmark to show collapse hint (on the last line of full content)
    local last_content_row = first_line_row + #content_lines - 1
    vim.api.nvim_buf_set_extmark(buf, thinking_ns, last_content_row, 0, {
      id = mark_id,
      virt_lines = { { { '[... press o to collapse]', 'TCodeTokens' } } },
    })

    entry.expanded = true
  end

  vim.bo[buf].modifiable = false
end

-- Render a single JSONL event into the buffer with extmarks
-- Serde externally-tagged enums: {"VariantName": {fields...}}
local function render_event(buf, ns, event)
  local variant, data = next(event)
  if not variant then return end

  if variant == 'UserMessage' then
    render_label(buf, ns, '► USER', '>>> USER', 'TCodeUser', data)
    local content_lines = vim.split(data.content, '\n', { plain = true })
    append_lines(buf, content_lines)

  elseif variant == 'AssistantMessageStart' then
    render_label(buf, ns, '► ASSISTANT', '>>> ASSISTANT', 'TCodeAssistant', data)
    append_lines(buf, { '' })

  elseif variant == 'AssistantThinkingChunk' then
    if not thinking_state.is_thinking then
      thinking_state.is_thinking = true
      thinking_state.start_row = vim.api.nvim_buf_line_count(buf) - 1  -- 0-indexed row where append_text writes
      thinking_state.content_parts = {}
      thinking_state.last_highlighted_row = thinking_state.start_row - 1
    end
    table.insert(thinking_state.content_parts, data.content)
    append_text(buf, data.content)
    -- Only highlight newly added lines (avoid O(n²) re-highlighting)
    local end_line = vim.api.nvim_buf_line_count(buf) - 1
    local from = thinking_state.last_highlighted_row + 1
    for i = from, end_line do
      vim.api.nvim_buf_add_highlight(buf, thinking_ns, 'TCodeThinking', i, 0, -1)
    end
    thinking_state.last_highlighted_row = end_line

  elseif variant == 'AssistantMessageChunk' then
    if thinking_state.is_thinking then
      collapse_thinking(buf, ns)
    end
    append_text(buf, data.content)

  elseif variant == 'AssistantMessageEnd' then
    -- Close the args fence on any still-generating tool calls. Uses per-entry
    -- extmark anchors so parallel tool calls each get their fence inserted at
    -- the right mid-buffer position (not stacked at end of buffer). State is
    -- kept around so the following ToolMessageStart / SubAgentStart can still
    -- find args_close_mark_id and the label extmark.
    for tool_call_id, _ in pairs(tool_call_gen_state) do
      close_args_fence(buf, tool_call_id)
    end
    -- Do NOT clear tool_call_gen_state here — ToolMessageStart still needs it.
    -- It will be cleaned up per-entry inside the ToolMessageStart handler.
    if thinking_state.is_thinking then
      collapse_thinking(buf, ns)
    end
    render_info(buf, ns, data, nil)

  elseif variant == 'AssistantToolCallStart' then
    -- Close any open thinking first
    collapse_thinking(buf, ns)

    local tool_name = data.tool_name or ''
    local tool_call_id = data.tool_call_id or ''
    local tool_call_index = data.tool_call_index or 0

    -- Render the tool label line
    local _, label_extmark = render_label(buf, ns, '► TOOL', '>>> TOOL: ' .. tool_name, 'TCodeTool', data)

    -- Store label info for status updates
    tc_tool_names[tool_call_id] = tool_name
    tc_label_marks[tool_call_id] = {
      extmark_id = label_extmark, ns = ns,
      tool_name = tool_name, created_at = data.created_at,
    }

    -- Show [generating] status with cancel hint
    update_tc_label(buf, tool_call_id, 'generating', 'TCodeTool', true)

    -- Open args fenced code block. Anchor an extmark to the OPENING fence row
    -- so we can locate the args region later even after other tool calls /
    -- collapses shift things around. Default right_gravity=true so that if
    -- another tool call's closing fence is inserted at this exact row from
    -- above (sibling close colliding with our open), the mark rides with the
    -- original fence character down to its new row instead of being left
    -- behind pointing at the sibling's close fence.
    append_lines(buf, { TC_FENCE, '' })
    local open_fence_row = vim.api.nvim_buf_line_count(buf) - 2
    local args_open_mark_id = vim.api.nvim_buf_set_extmark(buf, gen_ns, open_fence_row, 0, {})

    -- Store generation state (row positions resolved via extmarks, not stored)
    tool_call_gen_state[tool_call_id] = {
      args_open_mark_id = args_open_mark_id,
      args_close_mark_id = nil,
      content_parts = {},
      last_highlighted_row = nil,
      fence_closed = false,
    }
    tool_call_index_map[tool_call_index] = tool_call_id

  elseif variant == 'AssistantToolCallArgChunk' then
    local tool_call_index = data.tool_call_index or 0
    local tool_call_id = tool_call_index_map[tool_call_index]
    if tool_call_id and tool_call_gen_state[tool_call_id] then
      local state = tool_call_gen_state[tool_call_id]
      local content = tostring(data.content)

      -- Append text to buffer (streams into the open fence block)
      append_text(buf, content)
      table.insert(state.content_parts, content)

      -- Highlight new lines with TCodeToolArgs (same pattern as thinking chunks).
      -- Resolve the current args start row via extmark so that preceding
      -- edits (other tool calls above) don't invalidate the highlight range.
      local current_last_row = vim.api.nvim_buf_line_count(buf) - 1
      local args_open_row = get_gen_mark_row(buf, state.args_open_mark_id)
      local args_start_row = args_open_row and (args_open_row + 1) or current_last_row
      local start_hl = state.last_highlighted_row and (state.last_highlighted_row + 1) or args_start_row
      for row = start_hl, current_last_row do
        vim.api.nvim_buf_add_highlight(buf, -1, 'TCodeToolArgs', row, 0, -1)
      end
      state.last_highlighted_row = current_last_row
    end

  elseif variant == 'ToolMessageStart' then
    local tool_name = data.tool_name or ''
    local tool_call_id = data.tool_call_id or ''

    -- Check if we already have a generating state for this tool call
    local gen_state = tool_call_gen_state[tool_call_id]

    if gen_state then
      -- We were streaming args — close the args fence (idempotent — already
      -- handled by AssistantMessageEnd in most cases) and transition to
      -- [running].
      close_args_fence(buf, tool_call_id)

      -- Update label from [generating] to [running]
      update_tc_label(buf, tool_call_id, 'running', 'TCodeTool', true)

      -- Insert the tool-output opening fence + an empty output content row
      -- immediately after the closing args fence. NOT at end of buffer: with
      -- parallel tool calls this tool's region may be mid-buffer.
      local args_close_row = get_gen_mark_row(buf, gen_state.args_close_mark_id)
      local output_fence_row
      if args_close_row then
        output_fence_row = args_close_row + 1
        insert_lines_at(buf, output_fence_row, { TC_FENCE, '' })
      else
        -- Anchor lost — fall back to append at end of buffer.
        append_lines(buf, { TC_FENCE, '' })
        output_fence_row = vim.api.nvim_buf_line_count(buf) - 2
      end
      tc_fence_opened[tool_call_id] = true

      -- Resolve the label row via tc_label_marks extmark — the label may
      -- have shifted down since it was first rendered if other tool calls
      -- were inserted below, or since close_args_fence added a line.
      local label_info = tc_label_marks[tool_call_id]
      local label_row
      if label_info then
        local pos = vim.api.nvim_buf_get_extmark_by_id(buf, label_info.ns, label_info.extmark_id, {})
        label_row = pos and pos[1]
      end
      if not label_row then
        label_row = output_fence_row  -- degraded fallback
      end

      -- Navigation extmark: from the label row through the (empty) output
      -- content row so that ToolOutputChunk's extmark-based append finds
      -- the correct insert row.
      local nav_end_row = output_fence_row + 1
      local mark_id = vim.api.nvim_buf_set_extmark(buf, tc_ns, label_row, 0, {
        end_row = nav_end_row, end_col = 0,
      })
      tc_extmark_ids[mark_id] = tool_call_id

      -- Clean up gen state
      tool_call_gen_state[tool_call_id] = nil
    else
      -- Fallback: no streaming args (provider doesn't support it or missed events)
      -- Keep the original behavior
      local label_line, label_extmark = render_label(buf, ns, '► TOOL', '>>> TOOL: ' .. tool_name, 'TCodeTool', data)
      if data.tool_call_id then
        tc_tool_names[data.tool_call_id] = tool_name
        tc_label_marks[data.tool_call_id] = {
          extmark_id = label_extmark, ns = ns,
          tool_name = tool_name, created_at = data.created_at,
        }
        update_tc_label(buf, data.tool_call_id, 'running', 'TCodeTool', true)
      end
      if data.tool_args and data.tool_args ~= '' and data.tool_args ~= '{}' then
        -- Render tool input as real text lines (not virtual text) so the full
        -- content is visible and scrollable, wrapped in a fenced code block.
        local args_lines = vim.split(data.tool_args, '\n', { plain = true })
        append_lines(buf, { TC_FENCE })
        append_lines(buf, args_lines)
        -- Highlight the args lines with TCodeToolArgs
        local args_end = vim.api.nvim_buf_line_count(buf) - 1
        local args_start = args_end - #args_lines + 1
        for row = args_start, args_end do
          vim.api.nvim_buf_add_highlight(buf, -1, 'TCodeToolArgs', row, 0, -1)
        end
        append_lines(buf, { TC_FENCE })
      end
      -- Wrap tool output in a fenced code block to prevent markdown parser from
      -- interpreting partial HTML/XML, JSON, etc. as markdown syntax.
      append_lines(buf, { TC_FENCE, '' })
      -- Place a range extmark covering label through current last line
      if data.tool_call_id then
        tc_fence_opened[data.tool_call_id] = true
        local last_line = vim.api.nvim_buf_line_count(buf) - 1
        local mark_id = vim.api.nvim_buf_set_extmark(buf, tc_ns, label_line, 0, {
          end_row = last_line,
          end_col = 0,
        })
        tc_extmark_ids[mark_id] = data.tool_call_id
      end
    end

  elseif variant == 'ToolOutputChunk' then
    if data.tool_call_id then
      local end_row = get_tc_extmark_end_row(buf, data.tool_call_id)
      if end_row then
        local lines_before = vim.api.nvim_buf_line_count(buf)
        insert_text_at(buf, end_row, data.content)
        local lines_added = vim.api.nvim_buf_line_count(buf) - lines_before
        extend_tc_extmark(buf, data.tool_call_id, end_row + lines_added)
      else
        append_text(buf, data.content)
      end
    else
      append_text(buf, data.content)
    end

  elseif variant == 'ToolMessageEnd' then
    local insert_row = nil
    if data.tool_call_id then
      -- Close the fenced code block for tool output
      if tc_fence_opened[data.tool_call_id] then
        local end_row = get_tc_extmark_end_row(buf, data.tool_call_id)
        if end_row then
          insert_lines_at(buf, end_row + 1, { TC_FENCE })
          extend_tc_extmark(buf, data.tool_call_id, end_row + 1)
        end
        tc_fence_opened[data.tool_call_id] = nil
      end
      -- Find the row *after* the closing fence to insert info outside the code block
      local end_row = get_tc_extmark_end_row(buf, data.tool_call_id)
      if end_row then
        insert_row = end_row + 1
      end
      -- Update label with final status
      if tc_label_marks[data.tool_call_id] then
        local status_map = {
          Succeeded = { text = 'done', hl = 'TCodeSuccess' },
          Failed    = { text = 'failed', hl = 'TCodeError' },
          Cancelled = { text = 'cancelled', hl = 'TCodeError' },
          Timeout   = { text = 'failed', hl = 'TCodeError' },
          UserDenied = { text = 'denied', hl = 'TCodeError' },
        }
        local s = status_map[data.end_status] or { text = 'done', hl = 'TCodeSuccess' }
        update_tc_label(buf, data.tool_call_id, s.text, s.hl, false)
        tc_label_marks[data.tool_call_id] = nil
      end
    end
    local lines_before = vim.api.nvim_buf_line_count(buf)
    render_info(buf, ns, data, 'TOOL', insert_row)
    if data.tool_call_id and insert_row then
      local lines_added = vim.api.nvim_buf_line_count(buf) - lines_before
      extend_tc_extmark(buf, data.tool_call_id, insert_row + lines_added)
    end

  elseif variant == 'ToolRequestPermission' then
    if data.tool_call_id then
      update_tc_label(buf, data.tool_call_id, 'permission', 'TCodePermission', false)
    end

  elseif variant == 'ToolPermissionApproved' then
    if data.tool_call_id then
      update_tc_label(buf, data.tool_call_id, 'running', 'TCodeTool', true)
    end

  elseif variant == 'SystemMessage' then
    -- Display system message with appropriate styling based on level
    local level = data.level or 'Info'
    local prefix = '[' .. level:upper() .. '] '
    local hl_group = 'TCodeSystemInfo'
    local notify_level = vim.log.levels.INFO
    if level == 'Warning' then
      hl_group = 'TCodeSystemWarning'
      notify_level = vim.log.levels.WARN
    elseif level == 'Error' then
      hl_group = 'TCodeSystemError'
      notify_level = vim.log.levels.ERROR
    end
    -- Show as nvim notification (ephemeral)
    vim.notify(data.message or '', notify_level, { title = 'TCode' })
    -- Also show in main display (persistent)
    append_lines(buf, { '► SYSTEM' })
    local sep_line = vim.api.nvim_buf_line_count(buf) - 1
    vim.api.nvim_buf_set_extmark(buf, ns, sep_line, 0, {
      virt_text = { { prefix, hl_group } },
      virt_text_pos = 'overlay',
    })
    local msg_lines = vim.split(data.message or '', '\n', { plain = true })
    append_lines(buf, msg_lines)
    local start_row = vim.api.nvim_buf_line_count(buf) - #msg_lines
    for i = 0, #msg_lines - 1 do
      vim.api.nvim_buf_add_highlight(buf, ns, hl_group, start_row + i, 0, -1)
    end

  elseif variant == 'SubAgentInputStart' then
    -- Close any open thinking first
    collapse_thinking(buf, ns)

    local tool_name = data.tool_name or ''
    local tool_call_id = data.tool_call_id or ''
    local tool_call_index = data.tool_call_index or 0

    -- Render the subagent label line (same style as SubAgentStart but with [generating])
    local _, label_extmark = render_label(buf, ns, '► SUBAGENT', '>>> SUB-AGENT: [generating]', 'TCodeTool', data)

    -- Store in a pending map keyed by tool_call_id
    sa_input_marks[tool_call_id] = {
      extmark_id = label_extmark, ns = ns,
      tool_name = tool_name,
    }

    -- Open args fenced code block and anchor an extmark to the opening fence
    -- row (same pattern as AssistantToolCallStart — default right_gravity so
    -- the mark rides with the fence character on sibling mid-row inserts).
    append_lines(buf, { TC_FENCE, '' })
    local open_fence_row = vim.api.nvim_buf_line_count(buf) - 2
    local args_open_mark_id = vim.api.nvim_buf_set_extmark(buf, gen_ns, open_fence_row, 0, {})

    -- Store generation state (reuse tool_call_gen_state keyed by tool_call_id)
    tool_call_gen_state[tool_call_id] = {
      args_open_mark_id = args_open_mark_id,
      args_close_mark_id = nil,
      content_parts = {},
      last_highlighted_row = nil,
      fence_closed = false,
    }
    tool_call_index_map[tool_call_index] = tool_call_id

  elseif variant == 'SubAgentInputChunk' then
    -- Same logic as AssistantToolCallArgChunk
    local tool_call_index = data.tool_call_index or 0
    local tool_call_id = tool_call_index_map[tool_call_index]
    if tool_call_id and tool_call_gen_state[tool_call_id] then
      local state = tool_call_gen_state[tool_call_id]
      local content = tostring(data.content)
      append_text(buf, content)
      table.insert(state.content_parts, content)
      -- Highlight with TCodeToolArgs. Resolve the current args start row via
      -- extmark so it's correct even if preceding edits shifted rows.
      local current_last_row = vim.api.nvim_buf_line_count(buf) - 1
      local args_open_row = get_gen_mark_row(buf, state.args_open_mark_id)
      local args_start_row = args_open_row and (args_open_row + 1) or current_last_row
      local start_hl = state.last_highlighted_row and (state.last_highlighted_row + 1) or args_start_row
      for row = start_hl, current_last_row do
        vim.api.nvim_buf_add_highlight(buf, -1, 'TCodeToolArgs', row, 0, -1)
      end
      state.last_highlighted_row = current_last_row
    end

  elseif variant == 'SubAgentStart' then
    local description = data.description or ''
    local tool_call_id = data.tool_call_id
    local conv_id = data.conversation_id

    -- Check if we have a pending input label from SubAgentInputStart
    local pending = tool_call_id and sa_input_marks[tool_call_id]
    if pending then
      -- Close the args fence from SubAgentInputStart if still open. This is
      -- idempotent — AssistantMessageEnd may have already closed it. Uses
      -- extmark anchors so the closing fence lands mid-buffer at the right
      -- row even with other parallel tool calls below.
      local gen_state = tool_call_id and tool_call_gen_state[tool_call_id]
      if gen_state then
        close_args_fence(buf, tool_call_id)
      end

      -- Update existing label from [generating] to [running] description.
      -- Resolve the label row via the pending extmark (shifts with edits).
      local virt = {
        { '>>> SUB-AGENT: ', 'TCodeTool' },
        { '[running]', 'TCodeTool' },
        { ' ' .. description, 'TCodeTool' },
      }
      local mark_pos = vim.api.nvim_buf_get_extmark_by_id(buf, pending.ns, pending.extmark_id, {})
      local label_row = mark_pos and mark_pos[1]
      if label_row then
        vim.api.nvim_buf_set_extmark(buf, pending.ns, label_row, mark_pos[2], {
          id = pending.extmark_id,
          virt_text = virt,
          virt_text_pos = 'overlay',
        })
      end

      -- Transfer to sa_label_marks for future updates (SubAgentTurnEnd, SubAgentEnd, etc.)
      sa_label_marks[conv_id] = { extmark_id = pending.extmark_id, ns = pending.ns, description = description }
      sa_input_marks[tool_call_id] = nil

      -- Insert a blank line for subagent output content. Subagent content
      -- is NOT wrapped in an output fence (only the input args are fenced),
      -- so we just insert a single empty row immediately after the closing
      -- args fence.
      local args_close_row = gen_state and get_gen_mark_row(buf, gen_state.args_close_mark_id)
      local blank_row
      if args_close_row then
        blank_row = args_close_row + 1
        insert_lines_at(buf, blank_row, { '' })
      else
        append_lines(buf, { '' })
        blank_row = vim.api.nvim_buf_line_count(buf) - 1
      end

      -- Clean up gen state (args_close_mark_id no longer needed).
      if tool_call_id then tool_call_gen_state[tool_call_id] = nil end

      -- Set up the sa_ns range extmark spanning label through the new blank row.
      if conv_id and label_row then
        local mark_id = vim.api.nvim_buf_set_extmark(buf, sa_ns, label_row, 0, {
          end_row = blank_row, end_col = 0,
        })
        sa_extmark_ids[mark_id] = conv_id
      end
    else
      -- No pending input (e.g., resumed session) — create label from scratch (existing logic)
      local label_line, label_extmark = render_label(buf, ns, '► SUBAGENT', '>>> SUB-AGENT: [running] ' .. description, 'TCodeTool', data)
      append_lines(buf, { '' })
      if conv_id then
        sa_label_marks[conv_id] = { extmark_id = label_extmark, ns = ns, description = description }
        local last_line = vim.api.nvim_buf_line_count(buf) - 1
        local mark_id = vim.api.nvim_buf_set_extmark(buf, sa_ns, label_line, 0, {
          end_row = last_line, end_col = 0,
        })
        sa_extmark_ids[mark_id] = conv_id
      end
    end

  elseif variant == 'SubAgentEnd' then
    -- Update the start label in-place to show completion
    if data.conversation_id and sa_label_marks[data.conversation_id] then
      local info = sa_label_marks[data.conversation_id]
      local status_text = (data.end_status and data.end_status ~= 'Succeeded') and data.end_status or 'done'
      local status_hl = (data.end_status and data.end_status ~= 'Succeeded') and 'TCodeError' or 'TCodeSuccess'
      local virt = {
        { '>>> SUB-AGENT: ', 'TCodeTool' },
        { '[' .. status_text .. ']', status_hl },
      }
      local ts = format_time(data.created_at)
      if ts then
        table.insert(virt, { '  ' .. ts, 'TCodeTokens' })
      end
      if data.input_tokens and data.output_tokens then
        table.insert(virt, {
          string.format('  [%d in / %d out]', data.input_tokens, data.output_tokens),
          'TCodeTokens',
        })
      end
      table.insert(virt, { ' ' .. info.description, 'TCodeTool' })
      local mark_pos = vim.api.nvim_buf_get_extmark_by_id(buf, info.ns, info.extmark_id, {})
      if mark_pos and mark_pos[1] then
        vim.api.nvim_buf_set_extmark(buf, info.ns, mark_pos[1], mark_pos[2], {
          id = info.extmark_id,
          virt_text = virt,
          virt_text_pos = 'overlay',
        })
      end
      sa_label_marks[data.conversation_id] = nil
    end
    -- Render error as real text if present (needs to be visible/copyable)
    if type(data.error) == 'string' and data.error ~= '' then
      local sa_end_row = data.conversation_id and get_sa_extmark_end_row(buf, data.conversation_id)
      if sa_end_row then
        insert_lines_at(buf, sa_end_row, { '' })
        local error_start_line = sa_end_row
        local error_lines = vim.split('Error: ' .. data.error, '\n', { plain = true })
        vim.api.nvim_buf_set_lines(buf, error_start_line, error_start_line + 1, false, error_lines)
        for i = 0, #error_lines - 1 do
          vim.api.nvim_buf_add_highlight(buf, ns, 'TCodeError', error_start_line + i, 0, -1)
        end
        extend_sa_extmark(buf, data.conversation_id, error_start_line + #error_lines)
      else
        append_lines(buf, { '' })
        local error_start_line = vim.api.nvim_buf_line_count(buf) - 1
        local error_lines = vim.split('Error: ' .. data.error, '\n', { plain = true })
        vim.api.nvim_buf_set_lines(buf, error_start_line, error_start_line + 1, false, error_lines)
        for i = 0, #error_lines - 1 do
          vim.api.nvim_buf_add_highlight(buf, ns, 'TCodeError', error_start_line + i, 0, -1)
        end
      end
    end

  elseif variant == 'SubAgentTurnEnd' then
    -- Update the start label in-place to show idle status (do NOT clear from sa_label_marks)
    if data.conversation_id and sa_label_marks[data.conversation_id] then
      local info = sa_label_marks[data.conversation_id]
      local status_hl = (data.end_status and data.end_status ~= 'Succeeded') and 'TCodeError' or 'TCodeTokens'
      local status_text = (data.end_status and data.end_status ~= 'Succeeded') and data.end_status or 'idle'
      local virt = {
        { '>>> SUB-AGENT: ', 'TCodeTool' },
        { '[' .. status_text .. ']', status_hl },
      }
      if data.input_tokens and data.output_tokens then
        table.insert(virt, {
          string.format('  [%d in / %d out]', data.input_tokens, data.output_tokens),
          'TCodeTokens',
        })
      end
      table.insert(virt, { ' ' .. info.description, 'TCodeTool' })
      local mark_pos = vim.api.nvim_buf_get_extmark_by_id(buf, info.ns, info.extmark_id, {})
      if mark_pos and mark_pos[1] then
        vim.api.nvim_buf_set_extmark(buf, info.ns, mark_pos[1], mark_pos[2], {
          id = info.extmark_id,
          virt_text = virt,
          virt_text_pos = 'overlay',
        })
      end
    end

  elseif variant == 'SubAgentContinue' then
    local tool_call_id = data.tool_call_id

    -- Clean up any pending input state from SubAgentInputStart
    if tool_call_id and sa_input_marks[tool_call_id] then
      -- Close/collapse the args fence via extmark-anchored close (idempotent).
      if tool_call_gen_state[tool_call_id] then
        close_args_fence(buf, tool_call_id)
        tool_call_gen_state[tool_call_id] = nil
      end
      sa_input_marks[tool_call_id] = nil
    end

    -- Update existing label to show running again
    if data.conversation_id and sa_label_marks[data.conversation_id] then
      local info = sa_label_marks[data.conversation_id]
      local virt = {
        { '>>> SUB-AGENT: ', 'TCodeTool' },
        { '[running]', 'TCodeTool' },
        { ' ' .. info.description, 'TCodeTool' },
      }
      local mark_pos = vim.api.nvim_buf_get_extmark_by_id(buf, info.ns, info.extmark_id, {})
      if mark_pos and mark_pos[1] then
        vim.api.nvim_buf_set_extmark(buf, info.ns, mark_pos[1], mark_pos[2], {
          id = info.extmark_id,
          virt_text = virt,
          virt_text_pos = 'overlay',
        })
      end
    end

  elseif variant == 'SubAgentWaitingPermission' then
    if data.conversation_id and sa_label_marks[data.conversation_id] then
      local info = sa_label_marks[data.conversation_id]
      local virt = {
        { '>>> SUB-AGENT: ', 'TCodeTool' },
        { '[permission]', 'TCodePermission' },
        { ' ' .. info.description, 'TCodeTool' },
      }
      local mark_pos = vim.api.nvim_buf_get_extmark_by_id(buf, info.ns, info.extmark_id, {})
      if mark_pos and mark_pos[1] then
        vim.api.nvim_buf_set_extmark(buf, info.ns, mark_pos[1], mark_pos[2], {
          id = info.extmark_id,
          virt_text = virt,
          virt_text_pos = 'overlay',
        })
      end
    end

  -- Both Approved and Denied resolve a pending permission request and put
  -- the subagent label back into the [running] state.
  elseif variant == 'SubAgentPermissionApproved' or variant == 'SubAgentPermissionDenied' then
    if data.conversation_id and sa_label_marks[data.conversation_id] then
      local info = sa_label_marks[data.conversation_id]
      local virt = {
        { '>>> SUB-AGENT: ', 'TCodeTool' },
        { '[running]', 'TCodeTool' },
        { ' ' .. info.description, 'TCodeTool' },
      }
      local mark_pos = vim.api.nvim_buf_get_extmark_by_id(buf, info.ns, info.extmark_id, {})
      if mark_pos and mark_pos[1] then
        vim.api.nvim_buf_set_extmark(buf, info.ns, mark_pos[1], mark_pos[2], {
          id = info.extmark_id,
          virt_text = virt,
          virt_text_pos = 'overlay',
        })
      end
    end

  elseif variant == 'AssistantRequestEnd' then
    append_lines(buf, { '► END' })
    local info_line = vim.api.nvim_buf_line_count(buf) - 1
    local text
    -- Same semantics as render_info: "in" = processed, "cache read" = served from cache
    local total_cache_read = data.total_cache_read_tokens or 0
    local total_processed_input = (data.total_input_tokens or 0) + (data.total_cache_creation_tokens or 0)
    local total_output = data.total_output_tokens or 0
    if total_cache_read > 0 then
      text = string.format('[Total: %d in / %d cache read / %d out tokens]',
        total_processed_input, total_cache_read, total_output)
    else
      text = string.format('[Total: %d in / %d out tokens]',
        total_processed_input, total_output)
    end
    vim.api.nvim_buf_set_extmark(buf, ns, info_line, 0, {
      virt_text = { { text, 'TCodeTokens' } },
      virt_text_pos = 'overlay',
    })
  end
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
  local state = { last_size = 0, line_buffer = '' }

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

      local was_at_bottom = true
      local win = vim.fn.bufwinid(buf)
      if win ~= -1 then
        local cursor_line = vim.api.nvim_win_get_cursor(win)[1]
        local line_count = vim.api.nvim_buf_line_count(buf)
        was_at_bottom = (cursor_line >= line_count)
      end

      vim.bo[buf].modifiable = true

      for _, line in ipairs(lines) do
        if line ~= '' then
          local ok, event = pcall(vim.json.decode, line)
          if ok and event then
            if on_event then
              local variant, event_data = next(event)
              local ev_ok, ev_err = pcall(on_event, variant, event_data)
              if not ev_ok then
                vim.api.nvim_err_writeln('on_event error: ' .. tostring(ev_err))
              end
            end
            local render_ok, render_err = pcall(render_event, buf, ns, event)
            if not render_ok then
              vim.api.nvim_err_writeln('render_event error: ' .. tostring(render_err))
              break
            end
          end
        end
      end

      if win ~= -1 and was_at_bottom then
        local last_line_nr = vim.api.nvim_buf_line_count(buf)
        local last_line_text = vim.api.nvim_buf_get_lines(buf, last_line_nr - 1, last_line_nr, false)[1] or ''
        -- Set cursor to end of last line so viewport scrolls to show the latest
        -- content even when streaming appends to a long wrapped line.
        pcall(vim.api.nvim_win_set_cursor, win, { last_line_nr, #last_line_text })
      end

      if vim.api.nvim_buf_is_valid(buf) then
        vim.bo[buf].modifiable = false
      end

      -- Kick render-markdown.nvim once per batch, AFTER all events in this
      -- batch have been applied to the buffer. With debounce overridden to
      -- 0 in setup_display, this schedules an immediate repaint that sees
      -- the post-batch buffer state and applies fence conceals before the
      -- user's eyes can notice the raw backticks.
      force_render_markdown(buf)
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
    '%s --session=%s approve-next', M.exe_path, M.session_id))
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
function M.setup_display(display_file, status_file, usage_file, token_usage_file, session_id, exe_path, parser_path, runtime_dir)
  M.display_file = display_file or '/tmp/tcode-display.jsonl'
  M.status_file = status_file or '/tmp/tcode-status.txt'
  M.usage_file = usage_file
  M.token_usage_file = token_usage_file
  M.session_id = session_id
  M.exe_path = exe_path

  vim.g.tcode_status = 'Connecting...'
  vim.g.tcode_usage = ''
  vim.g.tcode_token_usage = ''
  vim.g.tcode_combined_usage = ''

  local function update_combined_usage()
    local parts = {}
    if vim.g.tcode_token_usage ~= '' then table.insert(parts, vim.g.tcode_token_usage) end
    if vim.g.tcode_usage ~= '' then table.insert(parts, vim.g.tcode_usage) end
    vim.g.tcode_combined_usage = table.concat(parts, ' │ ')
  end

  setup_highlights('#98c379', 114)
  disable_conflicting_plugins()
  local buf = create_display_buffer('tcode',
    '%#TCodeStatusLine# TCode: %{g:tcode_status}%=%{g:tcode_combined_usage} ')
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

  -- Reset first_event flag for this display session
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

  local check_updates = create_jsonl_reader(M.display_file, buf, ns)
  M.display_watcher = watch_file(M.display_file, check_updates)
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

  -- Clean up watchers when buffer is deleted or wiped
  vim.api.nvim_create_autocmd({'BufDelete', 'BufWipeout'}, {
    buffer = buf,
    callback = function()
      if M.display_watcher then M.display_watcher.stop(); M.display_watcher = nil end
      if M.status_watcher then M.status_watcher.stop(); M.status_watcher = nil end
      if M.usage_watcher then M.usage_watcher.stop(); M.usage_watcher = nil end
      if M.token_usage_watcher then M.token_usage_watcher.stop(); M.token_usage_watcher = nil end
    end,
  })

  vim.keymap.set('n', 'q', ':qa!<CR>', { buffer = true, silent = true, desc = 'Quit' })

  -- Context-aware 'o' keybinding: toggle thinking or open tool call detail
  vim.keymap.set('n', 'o', function()
    local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed

    -- Check for thinking extmark first
    local thinking_mark = find_thinking_at_line(buf, cursor_line)
    if thinking_mark then
      toggle_thinking(buf, thinking_mark)
      return
    end

    -- Priority 2: tool call args extmark → toggle expand/collapse
    local tool_args_mark = find_tool_args_at_line(buf, cursor_line)
    if tool_args_mark then
      toggle_tool_call_args(buf, tool_args_mark)
      return
    end

    -- Check for subagent extmark
    if M.exe_path and M.session_id then
      local sa_marks = vim.api.nvim_buf_get_extmarks(buf, sa_ns, 0, -1, { details = true })
      for _, mark in ipairs(sa_marks) do
        local start_row = mark[2]
        local details = mark[4]
        local end_row = details.end_row or start_row
        if cursor_line >= start_row and cursor_line <= end_row and sa_extmark_ids[mark[1]] then
          local conv_id = sa_extmark_ids[mark[1]]
          vim.fn.system(string.format('%s --session=%s open-subagent %s',
            M.exe_path, M.session_id, conv_id))
          return
        end
      end
    end

    -- Fall through to tool call detail
    if not M.exe_path or not M.session_id then
      vim.notify('Session info not available', vim.log.levels.ERROR)
      return
    end
    local marks = vim.api.nvim_buf_get_extmarks(buf, tc_ns, 0, -1, { details = true })
    local tool_call_id = nil
    for _, mark in ipairs(marks) do
      local start_row = mark[2]
      local details = mark[4]
      local end_row = details.end_row or start_row
      if cursor_line >= start_row and cursor_line <= end_row and tc_extmark_ids[mark[1]] then
        tool_call_id = tc_extmark_ids[mark[1]]
        break
      end
    end
    if not tool_call_id then
      return
    end
    vim.fn.system(string.format('%s --session=%s open-tool-call %s', M.exe_path, M.session_id, tool_call_id))
  end, { buffer = true, silent = true, desc = 'Open tool call detail' })

  -- Cancel tool or subagent with confirmation popup (Ctrl-k)
  -- Checks subagent first, then tool call.
  vim.keymap.set('n', '<C-k>', function()
    if not M.exe_path or not M.session_id then
      vim.notify('Session info not available', vim.log.levels.ERROR)
      return
    end

    local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed

    -- Check for subagent under cursor first
    local sa_marks = vim.api.nvim_buf_get_extmarks(buf, sa_ns, 0, -1, { details = true })
    for _, mark in ipairs(sa_marks) do
      local start_row = mark[2]
      local details = mark[4]
      local end_row = details.end_row or start_row
      if cursor_line >= start_row and cursor_line <= end_row and sa_extmark_ids[mark[1]] then
        local conv_id = sa_extmark_ids[mark[1]]
        if not sa_label_marks[conv_id] then
          vim.notify('Subagent already finished', vim.log.levels.INFO)
          return
        end
        local desc = sa_label_marks[conv_id].description or conv_id
        confirm_popup("Cancel subagent '" .. desc .. "'? (y/n)", function()
          local cmd = string.format('%s --session=%s cancel-conversation %s', M.exe_path, M.session_id, conv_id)
          local result = vim.fn.system(cmd)
          vim.notify(vim.trim(result), vim.log.levels.INFO, { title = 'TCode' })
        end)
        return
      end
    end

    -- Fall through to tool call cancel
    local marks = vim.api.nvim_buf_get_extmarks(buf, tc_ns, 0, -1, { details = true })
    local tool_call_id = nil
    for _, mark in ipairs(marks) do
      local start_row = mark[2]
      local details = mark[4]
      local end_row = details.end_row or start_row
      if cursor_line >= start_row and cursor_line <= end_row and tc_extmark_ids[mark[1]] then
        tool_call_id = tc_extmark_ids[mark[1]]
        break
      end
    end

    if not tool_call_id then
      vim.notify('No tool call or subagent under cursor', vim.log.levels.WARN)
      return
    end

    if not tc_label_marks[tool_call_id] then
      vim.notify('Tool call already finished', vim.log.levels.INFO)
      return
    end

    local tool_name = tc_tool_names[tool_call_id] or 'unknown'
    confirm_popup("Cancel tool '" .. tool_name .. "'? (y/n)", function()
      local cmd = string.format('%s --session=%s cancel-tool %s', M.exe_path, M.session_id, tool_call_id)
      local result = vim.fn.system(cmd)
      vim.notify(vim.trim(result), vim.log.levels.INFO, { title = 'TCode' })
    end)
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
      local cmd = string.format('%s --session=%s cancel-conversation %s', M.exe_path, M.session_id, conv_id)
      local result = vim.fn.system(cmd)
      vim.notify(vim.trim(result), vim.log.levels.INFO, { title = 'TCode' })
    end)
  end, { buffer = true, silent = true, desc = 'Cancel conversation' })

  -- Open pending tool approvals (Ctrl-P)
  vim.keymap.set('n', '<C-p>', open_pending_approvals,
    { buffer = true, silent = true, desc = 'Open pending tool approvals' })
end

-- Setup tool call display window for viewing a single tool call's details
-- @param tool_call_file: Path to the per-tool-call JSONL file
-- @param status_file: Path to the per-tool-call status file
function M.setup_tool_call_display(tool_call_file, status_file)
  M.tc_file = tool_call_file
  M.tc_status_file = status_file

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
-- Load shortcut templates from config (injected by Rust via _G.tcode_shortcuts)
-- Returns a table: { shortcut_name = "expanded text", ... }
-- Returns empty table if no shortcuts configured.
local function load_shortcuts()
  return _G.tcode_shortcuts or {}
end

-- Attempt to expand a /shortcut at the cursor position.
-- @param shortcuts: table of shortcut_name -> template_text
-- @param cursor_col: optional 0-indexed byte column (uses current cursor if nil)
-- Returns true if expanded, false otherwise.
local function try_expand_shortcut(shortcuts, cursor_col)
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

  local template = shortcuts[cmd]
  if not template then
    return false
  end

  -- Text before the /command and after cursor
  local prefix = line:sub(1, cmd_start - 1)
  local suffix = line:sub(col + 1)

  -- Split template into lines
  local replacement_lines = {}
  for tline in template:gmatch('[^\n]*') do
    table.insert(replacement_lines, tline)
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

-- Set up completion function for /shortcuts.
-- Called by nvim's insert-mode completion (<C-x><C-u>).
-- We wire <Tab> to trigger this when appropriate.
local function setup_shortcut_completion(shortcuts)
  -- Build sorted list of shortcut names for stable ordering
  local shortcut_names = {}
  for name, _ in pairs(shortcuts) do
    table.insert(shortcut_names, name)
  end
  table.sort(shortcut_names)

  -- Register the completefunc
  -- completefunc is called twice by nvim:
  --   1st call (findstart=1): return the column where the completion word starts
  --   2nd call (findstart=0): return the list of matches for `base`
  _G.tcode_shortcut_complete = function(findstart, base)
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
      -- Return matching shortcuts (base includes the '/')
      local prefix = base:match('^/(.*)') or ''
      local matches = {}
      for _, name in ipairs(shortcut_names) do
        if name:find(prefix, 1, true) == 1 then
          table.insert(matches, {
            word = '/' .. name,
            menu = shortcuts[name]:sub(1, 50) .. (shortcuts[name]:len() > 50 and '...' or ''),
          })
        end
      end
      return matches
    end
  end

  vim.bo.completefunc = 'v:lua.tcode_shortcut_complete'
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

  -- Load shortcuts from config
  local shortcuts = load_shortcuts()

  -- Set up shortcut keybindings if shortcuts are available
  if next(shortcuts) ~= nil then
    setup_shortcut_completion(shortcuts)

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
    end, { buffer = true, silent = true, desc = 'Auto-trigger shortcut completion' })

    -- <Tab> in insert mode: expand shortcut, show completion, or insert tab
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

      if cmd and shortcuts[cmd] then
        -- Exact match — expand the shortcut (pass col captured in insert mode)
        vim.cmd('stopinsert')
        try_expand_shortcut(shortcuts, col)
        vim.cmd('startinsert')
      elseif before_cursor:match('/%s*$') or cmd then
        -- Has / with partial or no text after it — trigger completion popup
        vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<C-x><C-u>', true, false, true), 'n', false)
      else
        -- No shortcut context — insert a normal tab
        vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Tab>', true, false, true), 'n', false)
      end
    end, { buffer = true, silent = true, desc = 'Expand shortcut or insert tab' })

    -- Auto-expand shortcut after selecting from completion popup
    vim.api.nvim_create_autocmd('CompleteDone', {
      buffer = 0,
      callback = function()
        local completed = vim.v.completed_item
        if completed and completed.word and completed.word:match('^/') then
          -- Schedule expansion to run after the completion popup closes
          vim.schedule(function()
            try_expand_shortcut(shortcuts)
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
