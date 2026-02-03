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

-- Setup display window for viewing conversation
-- @param display_file: Path to file where display content is written
-- @param status_file: Path to file where status messages are written
function M.setup_display(display_file, status_file)
  M.display_file = display_file or '/tmp/tcode-display.txt'
  M.status_file = status_file or '/tmp/tcode-status.txt'
  M.last_size = 0

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
  vim.bo.filetype = 'tcode'

  -- Set window options
  vim.wo.wrap = true
  vim.wo.linebreak = true
  vim.wo.number = false
  vim.wo.relativenumber = false
  vim.wo.signcolumn = 'no'

  -- Set statusline to show status
  vim.wo.statusline = '%#TCodeStatusLine# TCode: %{g:tcode_status} %='

  local buf = vim.api.nvim_get_current_buf()

  -- Function to check for new content
  local function check_updates()
    local file = io.open(M.display_file, 'r')
    if file then
      file:seek('set', M.last_size)
      local new_content = file:read('*all')
      file:close()

      if new_content and #new_content > 0 then
        M.last_size = M.last_size + #new_content

        vim.schedule(function()
          if vim.api.nvim_buf_is_valid(buf) then
            vim.bo[buf].modifiable = true

            local line_count = vim.api.nvim_buf_line_count(buf)
            local last_line = vim.api.nvim_buf_get_lines(buf, line_count - 1, line_count, false)[1] or ''
            local lines = vim.split(new_content, '\n', { plain = true })
            vim.api.nvim_buf_set_text(buf, line_count - 1, #last_line, line_count - 1, #last_line, lines)

            -- Scroll to bottom
            local win = vim.fn.bufwinid(buf)
            if win ~= -1 then
              local new_count = vim.api.nvim_buf_line_count(buf)
              vim.api.nvim_win_set_cursor(win, { new_count, 0 })
            end

            vim.bo[buf].modifiable = false
          end
        end)
      end
    end
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

  -- Set up basic syntax highlighting for messages
  vim.cmd([[
    syntax match TCodeUser /^>>> USER:/
    syntax match TCodeAssistant /^>>> ASSISTANT:/
    syntax match TCodeTool /^>>> TOOL:.*/

    highlight TCodeUser guifg=#61afef ctermfg=75
    highlight TCodeAssistant guifg=#98c379 ctermfg=114
    highlight TCodeTool guifg=#e5c07b ctermfg=180
    highlight TCodeStatusLine guibg=#282c34 guifg=#98c379 ctermfg=114 ctermbg=236
  ]])

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
