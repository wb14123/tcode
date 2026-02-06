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

-- Render a single JSONL event into the buffer with extmarks
-- Serde externally-tagged enums: {"VariantName": {fields...}}
local function render_event(buf, ns, event)
  local variant, data = next(event)
  if not variant then return end

  if variant == 'UserMessage' then
    append_lines(buf, { '' })
    local label_line = vim.api.nvim_buf_line_count(buf) - 1
    local virt = { { '>>> USER', 'TCodeUser' } }
    local ts = format_time(data.created_at)
    if ts then
      table.insert(virt, { '  ' .. ts, 'TCodeTokens' })
    end
    vim.api.nvim_buf_set_extmark(buf, ns, label_line, 0, {
      virt_text = virt,
      virt_text_pos = 'overlay',
    })
    local content_lines = vim.split(data.content, '\n', { plain = true })
    append_lines(buf, content_lines)

  elseif variant == 'AssistantMessageStart' then
    append_lines(buf, { '' })
    local label_line = vim.api.nvim_buf_line_count(buf) - 1
    local virt = { { '>>> ASSISTANT', 'TCodeAssistant' } }
    local ts = format_time(data.created_at)
    if ts then
      table.insert(virt, { '  ' .. ts, 'TCodeTokens' })
    end
    vim.api.nvim_buf_set_extmark(buf, ns, label_line, 0, {
      virt_text = virt,
      virt_text_pos = 'overlay',
    })
    -- Add empty line for content to append to (so chunks don't land on the label line)
    append_lines(buf, { '' })

  elseif variant == 'AssistantMessageChunk' then
    append_text(buf, data.content)

  elseif variant == 'AssistantMessageEnd' then
    append_lines(buf, { '' })
    local info_line = vim.api.nvim_buf_line_count(buf) - 1
    local parts = {}
    if data.input_tokens and data.output_tokens then
      table.insert(parts, {
        string.format('[%d in / %d out tokens]', data.input_tokens, data.output_tokens),
        'TCodeTokens',
      })
    end
    if data.end_status and data.end_status ~= 'Succeeded' then
      table.insert(parts, { ' [' .. data.end_status .. ']', 'TCodeError' })
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

  elseif variant == 'ToolMessageStart' then
    append_lines(buf, { '' })
    local label_line = vim.api.nvim_buf_line_count(buf) - 1
    local virt = { { '>>> TOOL: ' .. (data.tool_name or ''), 'TCodeTool' } }
    local ts = format_time(data.created_at)
    if ts then
      table.insert(virt, { '  ' .. ts, 'TCodeTokens' })
    end
    vim.api.nvim_buf_set_extmark(buf, ns, label_line, 0, {
      virt_text = virt,
      virt_text_pos = 'overlay',
    })
    if data.tool_args and data.tool_args ~= '' and data.tool_args ~= '{}' then
      append_lines(buf, { '' })
      local args_line = vim.api.nvim_buf_line_count(buf) - 1
      vim.api.nvim_buf_set_extmark(buf, ns, args_line, 0, {
        virt_text = { { data.tool_args, 'TCodeTokens' } },
        virt_text_pos = 'overlay',
      })
    end
    -- Add empty line for tool output to append to
    append_lines(buf, { '' })
    -- Place a range extmark covering label through current last line;
    -- ToolOutputChunk and ToolMessageEnd will extend end_row further.
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
    -- Extend the range extmark to cover newly appended output lines
    if data.tool_call_id then
      local last_line = vim.api.nvim_buf_line_count(buf) - 1
      local marks = vim.api.nvim_buf_get_extmarks(buf, tc_ns, 0, -1, {})
      for _, mark in ipairs(marks) do
        if tc_extmark_ids[mark[1]] == data.tool_call_id then
          vim.api.nvim_buf_set_extmark(buf, tc_ns, mark[2], mark[3], {
            id = mark[1],
            end_row = last_line,
            end_col = 0,
          })
          break
        end
      end
    end

  elseif variant == 'ToolMessageEnd' then
    append_lines(buf, { '' })
    local info_line = vim.api.nvim_buf_line_count(buf) - 1
    -- Extend the range extmark placed at ToolMessageStart to cover the whole tool block
    if data.tool_call_id then
      local marks = vim.api.nvim_buf_get_extmarks(buf, tc_ns, 0, -1, {})
      for _, mark in ipairs(marks) do
        if tc_extmark_ids[mark[1]] == data.tool_call_id then
          vim.api.nvim_buf_set_extmark(buf, tc_ns, mark[2], mark[3], {
            id = mark[1],
            end_row = info_line,
            end_col = 0,
          })
          break
        end
      end
    end
    local parts = {}
    if data.input_tokens and data.output_tokens and (data.input_tokens > 0 or data.output_tokens > 0) then
      table.insert(parts, {
        string.format('[%d in / %d out tokens]', data.input_tokens, data.output_tokens),
        'TCodeTokens',
      })
    end
    if data.end_status and data.end_status ~= 'Succeeded' then
      table.insert(parts, { ' [TOOL ' .. data.end_status .. ']', 'TCodeError' })
    end
    if #parts > 0 then
      vim.api.nvim_buf_set_extmark(buf, ns, info_line, 0, {
        virt_text = parts,
        virt_text_pos = 'overlay',
      })
    end

  elseif variant == 'SubAgentStart' then
    append_lines(buf, { '' })
    local label_line = vim.api.nvim_buf_line_count(buf) - 1
    vim.api.nvim_buf_set_extmark(buf, ns, label_line, 0, {
      virt_text = { { '>>> SUB-AGENT: ' .. (data.description or ''), 'TCodeTool' } },
      virt_text_pos = 'overlay',
    })

  elseif variant == 'SubAgentEnd' then
    append_lines(buf, { '' })
    local info_line = vim.api.nvim_buf_line_count(buf) - 1
    local parts = {}
    if data.input_tokens and data.output_tokens then
      table.insert(parts, {
        string.format('[sub-agent: %d in / %d out tokens]', data.input_tokens, data.output_tokens),
        'TCodeTokens',
      })
    end
    if data.end_status and data.end_status ~= 'Succeeded' then
      table.insert(parts, { ' [' .. data.end_status .. ']', 'TCodeError' })
    end
    if #parts > 0 then
      vim.api.nvim_buf_set_extmark(buf, ns, info_line, 0, {
        virt_text = parts,
        virt_text_pos = 'overlay',
      })
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
  M.last_size = 0
  M.line_buffer = ''

  -- Initialize status variable
  vim.g.tcode_status = 'Connecting...'

  -- Create a new empty buffer with a name
  vim.cmd('enew')
  vim.api.nvim_buf_set_name(0, '[TCode Display]')

  -- Configure buffer options
  vim.bo.buftype = 'nofile'
  vim.bo.bufhidden = 'hide'
  vim.bo.swapfile = false
  vim.bo.modifiable = false
  vim.bo.readonly = true
  vim.bo.filetype = 'markdown'

  -- Set window options
  vim.wo.wrap = true
  vim.wo.linebreak = true
  vim.wo.number = false
  vim.wo.relativenumber = false
  vim.wo.signcolumn = 'no'

  -- Set statusline to show status
  vim.wo.statusline = '%#TCodeStatusLine# TCode: %{g:tcode_status} %='

  -- Set up highlight groups
  vim.api.nvim_set_hl(0, 'TCodeUser', { fg = '#61afef', bold = true, ctermfg = 75 })
  vim.api.nvim_set_hl(0, 'TCodeAssistant', { fg = '#98c379', bold = true, ctermfg = 114 })
  vim.api.nvim_set_hl(0, 'TCodeTool', { fg = '#e5c07b', bold = true, ctermfg = 180 })
  vim.api.nvim_set_hl(0, 'TCodeTokens', { fg = '#5c6370', italic = true, ctermfg = 242 })
  vim.api.nvim_set_hl(0, 'TCodeError', { fg = '#e06c75', bold = true, ctermfg = 168 })
  vim.api.nvim_set_hl(0, 'TCodeStatusLine', { bg = '#282c34', fg = '#98c379', ctermfg = 114, ctermbg = 236 })

  local buf = vim.api.nvim_get_current_buf()
  local ns = vim.api.nvim_create_namespace('tcode')

  -- Function to check for new JSONL content
  local function check_updates()
    local file = io.open(M.display_file, 'r')
    if not file then return end
    file:seek('set', M.last_size)
    local new_content = file:read('*all')
    file:close()

    if not new_content or #new_content == 0 then return end
    M.last_size = M.last_size + #new_content

    -- Prepend any leftover partial line from last read
    local data = M.line_buffer .. new_content

    -- Split into lines; last element may be incomplete if data doesn't end with \n
    local lines = vim.split(data, '\n', { plain = true })
    if data:sub(-1) ~= '\n' then
      M.line_buffer = lines[#lines]
      table.remove(lines, #lines)
    else
      M.line_buffer = ''
    end

    vim.schedule(function()
      if not vim.api.nvim_buf_is_valid(buf) then return end

      -- Check if user is at the bottom before modifying content
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
            render_event(buf, ns, event)
          end
        end
      end

      -- Auto-scroll to bottom only if user was already at the bottom
      if win ~= -1 then
        local new_count = vim.api.nvim_buf_line_count(buf)
        if was_at_bottom then
          vim.api.nvim_win_set_cursor(win, { new_count, 0 })
        end
      end

      vim.bo[buf].modifiable = false
    end)
  end

  -- Function to check for status updates
  local function check_status()
    local file = io.open(M.status_file, 'r')
    if file then
      local status = file:read('*all')
      file:close()
      if status and status ~= '' then
        vim.schedule(function()
          if status == 'Shutdown' then
            vim.cmd('qa!')
            return
          end
          vim.g.tcode_status = status
          -- Force statusline redraw
          vim.cmd('redrawstatus')
        end)
      end
    end
  end

  -- Watch files for changes using inotify
  M.display_watcher = watch_file(M.display_file, check_updates)
  M.status_watcher = watch_file(M.status_file, check_status)

  -- Clean up watchers when buffer is deleted
  vim.api.nvim_create_autocmd('BufDelete', {
    buffer = buf,
    callback = function()
      if M.display_watcher then
        M.display_watcher.stop()
        M.display_watcher = nil
      end
      if M.status_watcher then
        M.status_watcher.stop()
        M.status_watcher = nil
      end
    end,
  })

  -- Add keybinding to quit
  vim.keymap.set('n', 'q', ':qa!<CR>', { buffer = true, silent = true, desc = 'Quit' })

  -- Add keybinding to open tool call detail in a new tmux tab
  vim.keymap.set('n', 'o', function()
    if not M.exe_path or not M.session_id then
      vim.notify('Session info not available', vim.log.levels.ERROR)
      return
    end
    local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1  -- 0-indexed
    -- Find extmarks whose range covers the cursor line
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
    local window_name = 'tool-detail'
    vim.fn.system(string.format('tmux new-window -n "%s" "%s"', window_name, cmd))
  end, { buffer = true, silent = true, desc = 'Open tool call detail' })
end

-- Setup tool call display window for viewing a single tool call's details
-- @param tool_call_file: Path to the per-tool-call JSONL file
-- @param status_file: Path to the per-tool-call status file
function M.setup_tool_call_display(tool_call_file, status_file)
  M.tc_file = tool_call_file
  M.tc_status_file = status_file
  M.tc_last_size = 0
  M.tc_line_buffer = ''

  -- Initialize status variable
  vim.g.tcode_tc_status = 'Waiting...'

  -- Create a new empty buffer with a name
  vim.cmd('enew')
  vim.api.nvim_buf_set_name(0, '[TCode Tool Call]')

  -- Configure buffer options
  vim.bo.buftype = 'nofile'
  vim.bo.bufhidden = 'hide'
  vim.bo.swapfile = false
  vim.bo.modifiable = false
  vim.bo.readonly = true
  vim.bo.filetype = 'markdown'

  -- Set window options
  vim.wo.wrap = true
  vim.wo.linebreak = true
  vim.wo.number = false
  vim.wo.relativenumber = false
  vim.wo.signcolumn = 'no'

  -- Set statusline to show tool call status
  vim.wo.statusline = '%#TCodeStatusLine# Tool Call: %{g:tcode_tc_status} %='

  -- Set up highlight groups (shared with main display)
  vim.api.nvim_set_hl(0, 'TCodeUser', { fg = '#61afef', bold = true, ctermfg = 75 })
  vim.api.nvim_set_hl(0, 'TCodeAssistant', { fg = '#98c379', bold = true, ctermfg = 114 })
  vim.api.nvim_set_hl(0, 'TCodeTool', { fg = '#e5c07b', bold = true, ctermfg = 180 })
  vim.api.nvim_set_hl(0, 'TCodeTokens', { fg = '#5c6370', italic = true, ctermfg = 242 })
  vim.api.nvim_set_hl(0, 'TCodeError', { fg = '#e06c75', bold = true, ctermfg = 168 })
  vim.api.nvim_set_hl(0, 'TCodeStatusLine', { bg = '#282c34', fg = '#e5c07b', ctermfg = 180, ctermbg = 236 })

  local buf = vim.api.nvim_get_current_buf()
  local ns = vim.api.nvim_create_namespace('tcode_tc')

  -- Function to check for new JSONL content
  local function check_updates()
    local file = io.open(M.tc_file, 'r')
    if not file then return end
    file:seek('set', M.tc_last_size)
    local new_content = file:read('*all')
    file:close()

    if not new_content or #new_content == 0 then return end
    M.tc_last_size = M.tc_last_size + #new_content

    -- Prepend any leftover partial line from last read
    local data = M.tc_line_buffer .. new_content

    -- Split into lines; last element may be incomplete if data doesn't end with \n
    local lines = vim.split(data, '\n', { plain = true })
    if data:sub(-1) ~= '\n' then
      M.tc_line_buffer = lines[#lines]
      table.remove(lines, #lines)
    else
      M.tc_line_buffer = ''
    end

    vim.schedule(function()
      if not vim.api.nvim_buf_is_valid(buf) then return end

      -- Check if user is at the bottom before modifying content
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
            -- Update status from tool events
            local variant, data_inner = next(event)
            if variant == 'ToolMessageStart' then
              vim.g.tcode_tc_status = 'Running: ' .. (data_inner.tool_name or '')
              vim.cmd('redrawstatus')
            elseif variant == 'ToolMessageEnd' then
              local status = data_inner.end_status or 'Unknown'
              vim.g.tcode_tc_status = 'Done: ' .. status
              vim.cmd('redrawstatus')
            end
            render_event(buf, ns, event)
          end
        end
      end

      -- Auto-scroll to bottom only if user was already at the bottom
      if win ~= -1 then
        local new_count = vim.api.nvim_buf_line_count(buf)
        if was_at_bottom then
          vim.api.nvim_win_set_cursor(win, { new_count, 0 })
        end
      end

      vim.bo[buf].modifiable = false
    end)
  end

  -- Function to check for status updates
  local function check_status()
    local file = io.open(M.tc_status_file, 'r')
    if file then
      local status = file:read('*all')
      file:close()
      if status and status ~= '' then
        vim.schedule(function()
          vim.cmd('redrawstatus')
        end)
      end
    end
  end

  -- Watch files for changes using inotify
  M.tc_watcher = watch_file(M.tc_file, check_updates)
  M.tc_status_watcher = watch_file(M.tc_status_file, check_status)

  -- Clean up watchers when buffer is deleted
  vim.api.nvim_create_autocmd('BufDelete', {
    buffer = buf,
    callback = function()
      if M.tc_watcher then
        M.tc_watcher.stop()
        M.tc_watcher = nil
      end
      if M.tc_status_watcher then
        M.tc_status_watcher.stop()
        M.tc_status_watcher = nil
      end
    end,
  })

  -- Add keybinding to quit
  vim.keymap.set('n', 'q', ':qa!<CR>', { buffer = true, silent = true, desc = 'Quit' })
end

-- Setup edit window for composing messages
-- @param msg_file: Path to file where messages should be written
function M.setup_edit(msg_file)
  -- Store the message file path
  M.msg_file = msg_file or '/tmp/tcode-edit-msg.txt'

  -- Create a new empty buffer with a name
  vim.cmd('enew')
  vim.api.nvim_buf_set_name(0, '[TCode Edit]')

  -- Configure buffer options
  -- 'acwrite' allows BufWriteCmd to handle :w
  vim.bo.buftype = 'acwrite'
  vim.bo.bufhidden = 'hide'
  vim.bo.swapfile = false
  vim.bo.filetype = 'markdown'

  -- Set window options
  vim.wo.wrap = true
  vim.wo.linebreak = true

  -- Set statusline
  vim.wo.statusline = '%#TCodeEditStatus# TCode Edit - Enter to send, o for new line %='

  -- Create autocmd to send content on save
  vim.api.nvim_create_autocmd('BufWriteCmd', {
    buffer = 0,
    callback = function()
      local buf = vim.api.nvim_get_current_buf()
      local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)

      -- Only send if there's actual content (not just comments/whitespace)
      local has_content = false
      for _, line in ipairs(lines) do
        if line:match('%S') and not line:match('^%-%-') then
          has_content = true
          break
        end
      end

      if has_content then
        -- Filter out comment lines and build final content
        local filtered_lines = {}
        for _, line in ipairs(lines) do
          if not line:match('^%-%-') then
            table.insert(filtered_lines, line)
          end
        end
        local filtered_content = table.concat(filtered_lines, '\n')

        -- Write content to message file
        local file = io.open(M.msg_file, 'w')
        if file then
          file:write(filtered_content)
          file:close()

          -- Clear the buffer
          vim.api.nvim_buf_set_lines(buf, 0, -1, false, {})

          -- Notify user
          vim.notify('Message sent!', vim.log.levels.INFO)
        else
          vim.notify('Failed to send message', vim.log.levels.ERROR)
        end
      end

      -- Mark buffer as not modified
      vim.bo[buf].modified = false
    end,
  })

  -- Add helpful keybindings
  vim.keymap.set('n', '<C-s>', ':w<CR>', { buffer = true, silent = true, desc = 'Send message' })
  -- Enter in insert mode to send (use 'o' to add new lines)
  vim.keymap.set('i', '<CR>', '<Esc>:w<CR>i', { buffer = true, silent = true, desc = 'Send message' })

  -- Set up highlight for statusline
  vim.cmd([[
    highlight TCodeEditStatus guibg=#282c34 guifg=#61afef ctermfg=75 ctermbg=236
  ]])

  -- Display instructions
  vim.api.nvim_buf_set_lines(0, 0, -1, false, {
    '-- Type message, Enter to send, o for new line',
    '',
  })
  vim.cmd('normal! G')
  vim.cmd('startinsert')
end

return M
