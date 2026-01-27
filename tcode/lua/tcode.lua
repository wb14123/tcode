local M = {}

-- Setup display window for viewing conversation
-- @param display_file: Path to file where display content is written
function M.setup_display(display_file)
  M.display_file = display_file or '/tmp/tcode-display.txt'
  M.last_size = 0

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

  local buf = vim.api.nvim_get_current_buf()

  -- Function to check for new content
  local function check_updates()
    local file = io.open(M.display_file, 'r')
    if file then
      local content = file:read('*all')
      file:close()

      local current_size = #content
      if current_size > M.last_size then
        -- Get new content
        local new_content = content:sub(M.last_size + 1)
        M.last_size = current_size

        -- Split into lines and append to buffer
        local lines = vim.split(new_content, '\n', { plain = true })

        vim.schedule(function()
          if vim.api.nvim_buf_is_valid(buf) then
            -- Temporarily make buffer modifiable
            vim.bo[buf].modifiable = true

            -- Get current last line
            local line_count = vim.api.nvim_buf_line_count(buf)
            local last_line = vim.api.nvim_buf_get_lines(buf, line_count - 1, line_count, false)[1] or ''

            -- If there are lines to add
            if #lines > 0 then
              -- Append first chunk to last line (for streaming)
              if lines[1] ~= '' then
                vim.api.nvim_buf_set_lines(buf, line_count - 1, line_count, false, { last_line .. lines[1] })
              end

              -- Add remaining lines
              if #lines > 1 then
                vim.api.nvim_buf_set_lines(buf, line_count, line_count, false, vim.list_slice(lines, 2))
              end

              -- Scroll to bottom
              local win = vim.fn.bufwinid(buf)
              if win ~= -1 then
                local new_count = vim.api.nvim_buf_line_count(buf)
                vim.api.nvim_win_set_cursor(win, { new_count, 0 })
              end
            end

            -- Make buffer read-only again
            vim.bo[buf].modifiable = false
          end
        end)
      end
    end
  end

  -- Create timer to poll for updates
  M.display_timer = vim.uv.new_timer()
  M.display_timer:start(100, 100, vim.schedule_wrap(check_updates))

  -- Clean up timer when buffer is deleted
  vim.api.nvim_create_autocmd('BufDelete', {
    buffer = buf,
    callback = function()
      if M.display_timer then
        M.display_timer:stop()
        M.display_timer:close()
        M.display_timer = nil
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

  -- Display instructions
  vim.api.nvim_buf_set_lines(0, 0, -1, false, {
    '-- Type your message below',
    '-- Press :w or Ctrl+S to send',
    '-- Lines starting with -- are ignored',
    '',
  })
  vim.cmd('normal! G')
end

return M
