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

-- Render a label line with optional timestamp as virtual text
local function render_label(buf, ns, prefix, hl_group, data)
  append_lines(buf, { '' })
  local label_line = vim.api.nvim_buf_line_count(buf) - 1
  local virt = { { prefix, hl_group } }
  local ts = format_time(data.created_at)
  if ts then
    table.insert(virt, { '  ' .. ts, 'TCodeTokens' })
  end
  vim.api.nvim_buf_set_extmark(buf, ns, label_line, 0, {
    virt_text = virt,
    virt_text_pos = 'overlay',
  })
  return label_line
end

-- Render a token/status info line as virtual text
local function render_info(buf, ns, data, token_prefix)
  append_lines(buf, { '' })
  local info_line = vim.api.nvim_buf_line_count(buf) - 1
  local parts = {}
  if data.input_tokens and data.output_tokens then
    local has_tokens = not token_prefix or (data.input_tokens > 0 or data.output_tokens > 0)
    if has_tokens then
      local fmt = token_prefix
        and string.format('[%s: %%d in / %%d out tokens]', token_prefix)
        or '[%d in / %d out tokens]'
      table.insert(parts, {
        string.format(fmt, data.input_tokens, data.output_tokens),
        'TCodeTokens',
      })
    end
  end
  if data.end_status and data.end_status ~= 'Succeeded' then
    local prefix = token_prefix and ' [' .. string.upper(token_prefix) .. ' ' or ' ['
    table.insert(parts, { prefix .. data.end_status .. ']', 'TCodeError' })
  end
  if type(data.error) == 'string' then
    table.insert(parts, { ' Error: ' .. data.error, 'TCodeError' })
  end
  if #parts > 0 then
    vim.api.nvim_buf_set_extmark(buf, ns, info_line, 0, {
      virt_text = parts,
      virt_text_pos = 'overlay',
    })
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

-- Render a single JSONL event into the buffer with extmarks
-- Serde externally-tagged enums: {"VariantName": {fields...}}
local function render_event(buf, ns, event)
  local variant, data = next(event)
  if not variant then return end

  if variant == 'UserMessage' then
    render_label(buf, ns, '>>> USER', 'TCodeUser', data)
    local content_lines = vim.split(data.content, '\n', { plain = true })
    append_lines(buf, content_lines)

  elseif variant == 'AssistantMessageStart' then
    render_label(buf, ns, '>>> ASSISTANT', 'TCodeAssistant', data)
    append_lines(buf, { '' })

  elseif variant == 'AssistantMessageChunk' then
    append_text(buf, data.content)

  elseif variant == 'AssistantMessageEnd' then
    render_info(buf, ns, data, nil)

  elseif variant == 'ToolMessageStart' then
    local label_line = render_label(buf, ns, '>>> TOOL: ' .. (data.tool_name or ''), 'TCodeTool', data)
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
    append_text(buf, data.content)
    if data.tool_call_id then
      extend_tc_extmark(buf, data.tool_call_id, vim.api.nvim_buf_line_count(buf) - 1)
    end

  elseif variant == 'ToolMessageEnd' then
    if data.tool_call_id then
      extend_tc_extmark(buf, data.tool_call_id, vim.api.nvim_buf_line_count(buf) - 1)
    end
    render_info(buf, ns, data, 'TOOL')

  elseif variant == 'SubAgentStart' then
    render_label(buf, ns, '>>> SUB-AGENT: ' .. (data.description or ''), 'TCodeTool', data)

  elseif variant == 'SubAgentEnd' then
    render_info(buf, ns, data, 'sub-agent')

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
  vim.api.nvim_set_hl(0, 'TCodeTokens', { fg = '#5c6370', italic = true, ctermfg = 242 })
  vim.api.nvim_set_hl(0, 'TCodeError', { fg = '#e06c75', bold = true, ctermfg = 168 })
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
  vim.bo.readonly = true
  vim.bo.filetype = 'markdown'

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
              on_event(variant, event_data)
            end
            render_event(buf, ns, event)
          end
        end
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

  -- Keybinding to open tool call detail in a new tmux tab
  vim.keymap.set('n', 'o', function()
    if not M.exe_path or not M.session_id then
      vim.notify('Session info not available', vim.log.levels.ERROR)
      return
    end
    local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed
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
      vim.notify('No tool call on this line', vim.log.levels.WARN)
      return
    end
    local cmd = string.format('%s --session=%s tool-call %s', M.exe_path, M.session_id, tool_call_id)
    vim.fn.system(string.format('tmux new-window -n "%s" "%s"', 'tool-detail', cmd))
  end, { buffer = true, silent = true, desc = 'Open tool call detail' })
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

  vim.api.nvim_buf_set_lines(0, 0, -1, false, {
    '-- Type message, Enter to send, o for new line',
    '',
  })
  vim.cmd('normal! G')
  vim.cmd('startinsert')
end

return M
