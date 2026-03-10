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

-- Append complete lines to the buffer
local function append_lines(buf, lines)
  local line_count = vim.api.nvim_buf_line_count(buf)
  vim.api.nvim_buf_set_lines(buf, line_count, line_count, false, lines)
end

-- Append text continuing from current buffer position (for streaming chunks)
local function append_text(buf, text)
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

-- Thinking token state: track streaming thinking and collapsed entries
local thinking_ns = vim.api.nvim_create_namespace('tcode_thinking')
local thinking_entries = {}  -- extmark_id -> { content, expanded }
local thinking_state = {
  is_thinking = false,
  start_row = nil,
  content_parts = {},  -- accumulate chunks in a table, concat only when needed
  last_highlighted_row = nil,  -- track last highlighted row to avoid re-highlighting
}

-- Treesitter markdown regions: only user/assistant message text is rendered as markdown.
-- We track line ranges and use set_included_regions() to restrict treesitter parsing.
local md_regions = {}   -- list of {start_row, end_row} (0-indexed)
local ts_parser = nil   -- treesitter LanguageTree, set in setup_display
local assistant_md = { started = false, start_row = nil, region_idx = nil }
local ts_batch_depth = 0  -- >0 means we're in a batch; defer treesitter parsing
local ts_dirty = false    -- set when update_ts_regions was deferred

--- Show a y/n confirmation popup at the cursor and execute callback on confirm.
local function confirm_popup(prompt, on_confirm)
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
  })

  local function close_popup()
    if vim.api.nvim_win_is_valid(popup_win) then
      vim.api.nvim_win_close(popup_win, true)
    end
    if vim.api.nvim_buf_is_valid(popup_buf) then
      vim.api.nvim_buf_delete(popup_buf, { force = true })
    end
  end

  vim.keymap.set('n', 'y', function()
    close_popup()
    on_confirm()
  end, { buffer = popup_buf, nowait = true })
  vim.keymap.set('n', 'n', close_popup, { buffer = popup_buf, nowait = true })
  vim.keymap.set('n', 'q', close_popup, { buffer = popup_buf, nowait = true })
  vim.keymap.set('n', '<Esc>', close_popup, { buffer = popup_buf, nowait = true })
end

local function flush_ts_regions(buf)
  if not ts_parser then return end
  if not vim.api.nvim_buf_is_valid(buf) then return end
  local regions = {}
  local total_lines = vim.api.nvim_buf_line_count(buf)
  for _, r in ipairs(md_regions) do
    local sr = math.max(0, r[1])
    local er = math.min(r[2], total_lines - 1)
    if er >= sr then
      local last_line = vim.api.nvim_buf_get_lines(buf, er, er + 1, false)[1] or ''
      table.insert(regions, { { sr, 0, er, #last_line } })
    end
  end
  pcall(function()
    ts_parser:set_included_regions(regions)
    ts_parser:parse(true)
  end)
  ts_dirty = false
end

local function update_ts_regions(buf)
  if ts_batch_depth > 0 then
    ts_dirty = true
    return
  end
  flush_ts_regions(buf)
end

-- Render a label line with optional timestamp as virtual text
local function render_label(buf, ns, prefix, hl_group, data)
  append_lines(buf, { '' })
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
local function render_info(buf, ns, data, token_prefix, insert_at)
  local info_line
  if insert_at then
    insert_lines_at(buf, insert_at, { '' })
    info_line = insert_at
  else
    append_lines(buf, { '' })
    info_line = vim.api.nvim_buf_line_count(buf) - 1
  end

  -- Collect virtual text parts for tokens/status metadata
  local virt_parts = {}
  if data.input_tokens and data.output_tokens then
    local has_tokens = not token_prefix or (data.input_tokens > 0 or data.output_tokens > 0)
    if has_tokens then
      local fmt = token_prefix
        and string.format('[%s: %%d in / %%d out tokens]', token_prefix)
        or '[%d in / %d out tokens]'
      table.insert(virt_parts, {
        string.format(fmt, data.input_tokens, data.output_tokens),
        'TCodeTokens',
      })
    end
  end
  if data.end_status and data.end_status ~= 'Succeeded' then
    local prefix = token_prefix and ' [' .. string.upper(token_prefix) .. ' ' or ' ['
    table.insert(virt_parts, { prefix .. data.end_status .. ']', 'TCodeError' })
  end

  -- Render tokens/status as virtual text
  if #virt_parts > 0 then
    vim.api.nvim_buf_set_extmark(buf, ns, info_line, 0, {
      virt_text = virt_parts,
      virt_text_pos = 'overlay',
    })
  end

  -- Render error as real buffer text so it can wrap, be navigated, selected, copied
  if type(data.error) == 'string' and data.error ~= '' then
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

-- Render a single JSONL event into the buffer with extmarks
-- Serde externally-tagged enums: {"VariantName": {fields...}}
local function render_event(buf, ns, event)
  local variant, data = next(event)
  if not variant then return end

  -- Auto-close an unclosed markdown region when a non-assistant event arrives
  if assistant_md.started
      and variant ~= 'AssistantMessageChunk'
      and variant ~= 'AssistantMessageEnd'
      and variant ~= 'AssistantThinkingChunk' then
    if assistant_md.region_idx then
      md_regions[assistant_md.region_idx][2] = vim.api.nvim_buf_line_count(buf) - 1
      update_ts_regions(buf)
    end
    assistant_md.started = false
    assistant_md.start_row = nil
    assistant_md.region_idx = nil
  end

  if variant == 'UserMessage' then
    render_label(buf, ns, '>>> USER', 'TCodeUser', data)
    local start_row = vim.api.nvim_buf_line_count(buf)
    local content_lines = vim.split(data.content, '\n', { plain = true })
    append_lines(buf, content_lines)
    local end_row = vim.api.nvim_buf_line_count(buf) - 1
    table.insert(md_regions, { start_row, end_row })
    update_ts_regions(buf)

  elseif variant == 'AssistantMessageStart' then
    render_label(buf, ns, '>>> ASSISTANT', 'TCodeAssistant', data)
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
    if not assistant_md.started then
      assistant_md.start_row = vim.api.nvim_buf_line_count(buf) - 1
      assistant_md.started = true
      table.insert(md_regions, { assistant_md.start_row, assistant_md.start_row })
      assistant_md.region_idx = #md_regions
    end
    append_text(buf, data.content)
    md_regions[assistant_md.region_idx][2] = vim.api.nvim_buf_line_count(buf) - 1
    update_ts_regions(buf)

  elseif variant == 'AssistantMessageEnd' then
    if thinking_state.is_thinking then
      collapse_thinking(buf, ns)
    end
    if assistant_md.started then
      md_regions[assistant_md.region_idx][2] = vim.api.nvim_buf_line_count(buf) - 1
      update_ts_regions(buf)
      assistant_md.started = false
      assistant_md.start_row = nil
      assistant_md.region_idx = nil
    end
    render_info(buf, ns, data, nil)

  elseif variant == 'ToolMessageStart' then
    local tool_name = data.tool_name or ''
    local label_line, label_extmark = render_label(buf, ns, '>>> TOOL: ' .. tool_name, 'TCodeTool', data)
    -- Store tool name and label extmark for cancel hint updates
    if data.tool_call_id then
      tc_tool_names[data.tool_call_id] = tool_name
      tc_label_marks[data.tool_call_id] = { extmark_id = label_extmark, ns = ns, tool_name = tool_name }
      -- Update label to include cancel hint
      local virt = { { '>>> TOOL: ' .. tool_name, 'TCodeTool' } }
      local ts = format_time(data.created_at)
      if ts then
        table.insert(virt, { '  ' .. ts, 'TCodeTokens' })
      end
      table.insert(virt, { '  [Ctrl-k to cancel]', 'TCodeTokens' })
      vim.api.nvim_buf_set_extmark(buf, ns, label_line, 0, {
        id = label_extmark,
        virt_text = virt,
        virt_text_pos = 'overlay',
      })
    end
    if data.tool_args and data.tool_args ~= '' and data.tool_args ~= '{}' then
      append_lines(buf, { '' })
      local args_line = vim.api.nvim_buf_line_count(buf) - 1
      vim.api.nvim_buf_set_extmark(buf, ns, args_line, 0, {
        virt_text = { { data.tool_args, 'TCodeTokens' } },
        virt_text_pos = 'overlay',
      })
    end
    append_lines(buf, { '' })
    -- Place a range extmark covering label through current last line
    if data.tool_call_id then
      local last_line = vim.api.nvim_buf_line_count(buf) - 1
      local mark_id = vim.api.nvim_buf_set_extmark(buf, tc_ns, label_line, 0, {
        end_row = last_line,
        end_col = 0,
      })
      tc_extmark_ids[mark_id] = data.tool_call_id
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
      -- Find the end_row of this tool's extmark to insert within its section
      local end_row = get_tc_extmark_end_row(buf, data.tool_call_id)
      if end_row then
        insert_row = end_row
      end
      -- Remove cancel hint from label
      if tc_label_marks[data.tool_call_id] then
        local info = tc_label_marks[data.tool_call_id]
        local virt = { { '>>> TOOL: ' .. info.tool_name, 'TCodeTool' } }
        local mark_pos = vim.api.nvim_buf_get_extmark_by_id(buf, info.ns, info.extmark_id, {})
        if mark_pos and mark_pos[1] then
          vim.api.nvim_buf_set_extmark(buf, info.ns, mark_pos[1], mark_pos[2], {
            id = info.extmark_id,
            virt_text = virt,
            virt_text_pos = 'overlay',
          })
        end
        tc_label_marks[data.tool_call_id] = nil
      end
    end
    local lines_before = vim.api.nvim_buf_line_count(buf)
    render_info(buf, ns, data, 'TOOL', insert_row)
    if data.tool_call_id and insert_row then
      local lines_added = vim.api.nvim_buf_line_count(buf) - lines_before
      extend_tc_extmark(buf, data.tool_call_id, insert_row + lines_added)
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
    append_lines(buf, { '' })
    local msg_lines = vim.split(prefix .. (data.message or ''), '\n', { plain = true })
    append_lines(buf, msg_lines)
    local start_row = vim.api.nvim_buf_line_count(buf) - #msg_lines
    for i = 0, #msg_lines - 1 do
      vim.api.nvim_buf_add_highlight(buf, ns, hl_group, start_row + i, 0, -1)
    end

  elseif variant == 'SubAgentStart' then
    local description = data.description or ''
    local label_line, label_extmark = render_label(buf, ns, '>>> SUB-AGENT: ' .. description .. ' [running]', 'TCodeTool', data)
    append_lines(buf, { '' })
    -- Place a range extmark covering label through current last line
    if data.conversation_id then
      sa_label_marks[data.conversation_id] = { extmark_id = label_extmark, ns = ns, description = description }
      local last_line = vim.api.nvim_buf_line_count(buf) - 1
      local mark_id = vim.api.nvim_buf_set_extmark(buf, sa_ns, label_line, 0, {
        end_row = last_line,
        end_col = 0,
      })
      sa_extmark_ids[mark_id] = data.conversation_id
    end

  elseif variant == 'SubAgentEnd' then
    -- Update the start label in-place to show completion
    if data.conversation_id and sa_label_marks[data.conversation_id] then
      local info = sa_label_marks[data.conversation_id]
      local status_text = (data.end_status and data.end_status ~= 'Succeeded') and data.end_status or 'done'
      local status_hl = (data.end_status and data.end_status ~= 'Succeeded') and 'TCodeError' or 'TCodeTool'
      local virt = {
        { '>>> SUB-AGENT: ' .. info.description .. ' ', 'TCodeTool' },
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
        { '>>> SUB-AGENT: ' .. info.description .. ' ', 'TCodeTool' },
        { '[' .. status_text .. ']', status_hl },
      }
      if data.input_tokens and data.output_tokens then
        table.insert(virt, {
          string.format('  [%d in / %d out]', data.input_tokens, data.output_tokens),
          'TCodeTokens',
        })
      end
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
    -- Update existing label to show running again
    if data.conversation_id and sa_label_marks[data.conversation_id] then
      local info = sa_label_marks[data.conversation_id]
      local virt = {
        { '>>> SUB-AGENT: ' .. info.description .. ' ', 'TCodeTool' },
        { '[running]', 'TCodeTool' },
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
    append_lines(buf, { '', '' })
    local info_line = vim.api.nvim_buf_line_count(buf) - 1
    if data.total_input_tokens and data.total_output_tokens then
      vim.api.nvim_buf_set_extmark(buf, ns, info_line, 0, {
        virt_text = { {
          string.format('[Total: %d in / %d out tokens]', data.total_input_tokens, data.total_output_tokens),
          'TCodeTokens',
        } },
        virt_text_pos = 'overlay',
      })
    end
  end
end

-- Set up highlight groups used by all display buffers
local function setup_highlights(statusline_fg, statusline_ctermfg)
  vim.api.nvim_set_hl(0, 'TCodeUser', { fg = '#61afef', bold = true, ctermfg = 75 })
  vim.api.nvim_set_hl(0, 'TCodeAssistant', { fg = '#98c379', bold = true, ctermfg = 114 })
  vim.api.nvim_set_hl(0, 'TCodeTool', { fg = '#e5c07b', bold = true, ctermfg = 180 })
  vim.api.nvim_set_hl(0, 'TCodeThinking', { fg = '#7c8495', italic = true, ctermfg = 245 })
  vim.api.nvim_set_hl(0, 'TCodeTokens', { fg = '#5c6370', italic = true, ctermfg = 242 })
  vim.api.nvim_set_hl(0, 'TCodeError', { fg = '#e06c75', bold = true, ctermfg = 168 })
  vim.api.nvim_set_hl(0, 'TCodeSystemInfo', { fg = '#61afef', italic = true, ctermfg = 75 })
  vim.api.nvim_set_hl(0, 'TCodeSystemWarning', { fg = '#e5c07b', bold = true, ctermfg = 180 })
  vim.api.nvim_set_hl(0, 'TCodeSystemError', { fg = '#e06c75', bold = true, ctermfg = 168 })
  vim.api.nvim_set_hl(0, 'TCodeStatusLine', {
    bg = '#282c34', fg = statusline_fg,
    ctermfg = statusline_ctermfg, ctermbg = 236,
  })
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

      -- Batch mode: defer treesitter parsing until after all events are processed
      ts_batch_depth = ts_batch_depth + 1

      for _, line in ipairs(lines) do
        if line ~= '' then
          local ok, event = pcall(vim.json.decode, line)
          if ok and event then
            if on_event then
              local variant, event_data = next(event)
              on_event(variant, event_data)
            end
            render_event(buf, ns, event)
          end
        end
      end

      ts_batch_depth = ts_batch_depth - 1
      if ts_batch_depth == 0 and ts_dirty then
        flush_ts_regions(buf)
      end

      if win ~= -1 and was_at_bottom then
        vim.api.nvim_win_set_cursor(win, { vim.api.nvim_buf_line_count(buf), 0 })
      end

      vim.bo[buf].modifiable = false
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

-- Setup display window for viewing conversation
-- @param display_file: Path to file where display content is written (JSONL)
-- @param status_file: Path to file where status messages are written
-- @param session_id: Session ID for spawning tool call windows
-- @param exe_path: Path to tcode executable
function M.setup_display(display_file, status_file, session_id, exe_path)
  M.display_file = display_file or '/tmp/tcode-display.jsonl'
  M.status_file = status_file or '/tmp/tcode-status.txt'
  M.session_id = session_id
  M.exe_path = exe_path

  vim.g.tcode_status = 'Connecting...'

  setup_highlights('#98c379', 114)
  local buf = create_display_buffer('[TCode Display]',
    '%#TCodeStatusLine# TCode: %{g:tcode_status} %=')
  local ns = vim.api.nvim_create_namespace('tcode')

  -- Use treesitter to render only user/assistant content as markdown.
  -- set_included_regions() restricts parsing to tracked line ranges.
  pcall(function()
    ts_parser = vim.treesitter.get_parser(buf, 'markdown')
    vim.treesitter.start(buf, 'markdown')
    ts_parser:set_included_regions({})
  end)

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

  -- Clean up watchers when buffer is deleted
  vim.api.nvim_create_autocmd('BufDelete', {
    buffer = buf,
    callback = function()
      if M.display_watcher then M.display_watcher.stop(); M.display_watcher = nil end
      if M.status_watcher then M.status_watcher.stop(); M.status_watcher = nil end
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

    -- Check for subagent extmark
    if M.exe_path and M.session_id then
      local sa_marks = vim.api.nvim_buf_get_extmarks(buf, sa_ns, 0, -1, { details = true })
      for _, mark in ipairs(sa_marks) do
        local start_row = mark[2]
        local details = mark[4]
        local end_row = details.end_row or start_row
        if cursor_line >= start_row and cursor_line <= end_row and sa_extmark_ids[mark[1]] then
          local conv_id = sa_extmark_ids[mark[1]]
          local sa_session = M.session_id .. '/subagent-' .. conv_id
          local cmd = string.format('%s --session=%s display', M.exe_path, sa_session)
          vim.fn.system(string.format('tmux new-window -n "%s" "%s"', 'subagent', cmd))
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
    local cmd = string.format('%s --session=%s tool-call %s', M.exe_path, M.session_id, tool_call_id)
    vim.fn.system(string.format('tmux new-window -n "%s" "%s"', 'tool-detail', cmd))
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
end

-- Setup tool call display window for viewing a single tool call's details
-- @param tool_call_file: Path to the per-tool-call JSONL file
-- @param status_file: Path to the per-tool-call status file
function M.setup_tool_call_display(tool_call_file, status_file)
  M.tc_file = tool_call_file
  M.tc_status_file = status_file

  vim.g.tcode_tc_status = 'Waiting...'

  setup_highlights('#e5c07b', 180)
  local buf = create_display_buffer('[TCode Tool Call]',
    '%#TCodeStatusLine# Tool Call: %{g:tcode_tc_status} %=')
  local ns = vim.api.nvim_create_namespace('tcode_tc')

  local check_updates = create_jsonl_reader(M.tc_file, buf, ns, function(variant, data)
    if variant == 'ToolMessageStart' then
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
-- @param msg_file: Path to file where messages should be written
function M.setup_edit(msg_file)
  M.msg_file = msg_file or '/tmp/tcode-edit-msg.txt'

  vim.cmd('enew')
  vim.api.nvim_buf_set_name(0, '[TCode Edit]')

  vim.bo.buftype = 'acwrite'
  vim.bo.bufhidden = 'hide'
  vim.bo.swapfile = false
  vim.bo.filetype = 'markdown'

  vim.wo.wrap = true
  vim.wo.linebreak = true

  vim.wo.statusline = '%#TCodeEditStatus# TCode Edit - Enter to send, o for new line %='

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
          vim.notify('Message sent!', vim.log.levels.INFO)
        else
          vim.notify('Failed to send message', vim.log.levels.ERROR)
        end
      end

      vim.bo[buf].modified = false
    end,
  })

  vim.keymap.set('n', '<C-s>', ':w<CR>', { buffer = true, silent = true, desc = 'Send message' })
  vim.keymap.set('i', '<CR>', '<Esc>:w<CR>i', { buffer = true, silent = true, desc = 'Send message' })

  vim.cmd([[
    highlight TCodeEditStatus guibg=#282c34 guifg=#61afef ctermfg=75 ctermbg=236
  ]])

  vim.api.nvim_buf_set_lines(0, 0, -1, false, { '' })
  vim.cmd('startinsert')
end

return M
