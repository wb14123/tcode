-- Session name tests: the `gn` input popup, the set-name command it builds,
-- and the stored-name prefill helper. The CLI shell-out is stubbed the same way
-- as in regression_tests.lua, so no server runs here.

-- A fixture directory under the harness tmp dir (the runner removes it).
local function fixture_dir(tag)
  local dir = tmp_dir .. '/session-name-' .. tag
  vim.fn.mkdir(dir, 'p')
  return dir
end

local function write_file(path, content)
  local file = assert(io.open(path, 'w'))
  file:write(content)
  file:close()
end

-- Stub vim.fn.system and the session info, run fn, then restore both before
-- any check runs. Returns the command strings fn caused.
local function with_stubbed_system(fn)
  local orig_system = vim.fn.system
  local orig_exe = M.exe_path
  local orig_session = M.session_id
  M.exe_path = '/fake/tcode'
  M.session_id = 'sess1'
  local calls = {}
  vim.fn.system = function(cmd)
    calls[#calls + 1] = cmd
    return ''
  end
  local ok, err = pcall(fn, calls)
  vim.fn.system = orig_system
  M.exe_path = orig_exe
  M.session_id = orig_session
  return calls, ok, err
end

test('session name: Enter submits the typed name as a set-name command', function()
  local dir = fixture_dir('set')
  local handle = nil
  local calls, ok, err = with_stubbed_system(function()
    handle = T.keymap_gn(dir, false)
    if not handle then return end
    vim.api.nvim_buf_set_lines(handle.buf, 0, -1, false, { ' my session ' })
    handle.accept()
  end)
  local closed = handle ~= nil and not vim.api.nvim_win_is_valid(handle.win)
  if handle and vim.api.nvim_win_is_valid(handle.win) then handle.cancel() end
  check(handle ~= nil, 'gn opens the input popup')
  check(ok, 'accept runs without error: ' .. tostring(err))
  check(#calls == 1, 'one set-name shell-out')
  local cmd = calls[1] or ''
  check(cmd:find("set-name -- 'my session' 2>&1", 1, true) ~= nil,
    'the command carries -- and the trimmed, quoted name')
  check(cmd:find("sess1", 1, true) ~= nil, 'the command carries the session id')
  check(cmd:find('--clear', 1, true) == nil, 'a set command never clears')
  check(closed, 'accept closes the popup')
end)

test('session name: a name that starts with a dash goes after the separator', function()
  local dir = fixture_dir('leading-dash')
  local handle = nil
  local calls, ok, err = with_stubbed_system(function()
    handle = T.keymap_gn(dir, false)
    if not handle then return end
    vim.api.nvim_buf_set_lines(handle.buf, 0, -1, false, { ' -leading ' })
    handle.accept()
  end)
  if handle and vim.api.nvim_win_is_valid(handle.win) then handle.cancel() end
  check(ok, 'accept runs without error: ' .. tostring(err))
  check(#calls == 1, 'one set-name shell-out')
  local cmd = calls[1] or ''
  check(cmd:find("set-name -- '-leading' 2>&1", 1, true) ~= nil,
    'the separator stops the parser from reading the dash as a flag')
end)

test('session name: an empty input clears the name', function()
  local dir = fixture_dir('clear-empty')
  local handle = nil
  local calls, ok, err = with_stubbed_system(function()
    handle = T.keymap_gn(dir, false)
    if not handle then return end
    handle.accept()
  end)
  if handle and vim.api.nvim_win_is_valid(handle.win) then handle.cancel() end
  check(ok, 'accept runs without error: ' .. tostring(err))
  check(#calls == 1, 'one set-name shell-out')
  local cmd = calls[1] or ''
  check(cmd:find('set-name --clear 2>&1', 1, true) ~= nil, 'an empty input clears')
  check(cmd:find("sess1", 1, true) ~= nil, 'the clear command carries the session id')
end)

test('session name: a spaces-only input clears the name', function()
  local dir = fixture_dir('clear-spaces')
  local handle = nil
  local calls, ok, err = with_stubbed_system(function()
    handle = T.keymap_gn(dir, false)
    if not handle then return end
    vim.api.nvim_buf_set_lines(handle.buf, 0, -1, false, { '   ' })
    handle.accept()
  end)
  if handle and vim.api.nvim_win_is_valid(handle.win) then handle.cancel() end
  check(ok, 'accept runs without error: ' .. tostring(err))
  check(#calls == 1, 'one set-name shell-out')
  local cmd = calls[1] or ''
  check(cmd:find('set-name --clear 2>&1', 1, true) ~= nil, 'spaces only clears')
end)

test('session name: Esc discards the input and runs no command', function()
  local dir = fixture_dir('cancel')
  local handle = nil
  local closed = false
  local calls, ok, err = with_stubbed_system(function()
    handle = T.keymap_gn(dir, false)
    if not handle then return end
    vim.api.nvim_buf_set_lines(handle.buf, 0, -1, false, { 'dropped' })
    handle.cancel()
    closed = not vim.api.nvim_win_is_valid(handle.win)
  end)
  if handle and vim.api.nvim_win_is_valid(handle.win) then handle.cancel() end
  check(ok, 'cancel runs without error: ' .. tostring(err))
  check(closed, 'cancel closes the popup')
  check(#calls == 0, 'Esc runs no command')
end)

test('session name: the input opens with the stored name', function()
  local dir = fixture_dir('prefill-open')
  write_file(dir .. '/session-meta.json', '{"name":"stored name","description":"desc"}')
  local handle = nil
  local line = nil
  local calls, ok, err = with_stubbed_system(function()
    handle = T.keymap_gn(dir, true)
    if not handle then return end
    line = vim.api.nvim_buf_get_lines(handle.buf, 0, -1, false)[1]
    handle.cancel()
  end)
  if handle and vim.api.nvim_win_is_valid(handle.win) then handle.cancel() end
  check(ok, 'the keybinding runs without error: ' .. tostring(err))
  check(line == 'stored name', 'the popup starts with the stored name, got ' .. tostring(line))
  check(#calls == 0, 'opening the input runs no command')
end)

test('session name: the input popup carries a title hint', function()
  local dir = fixture_dir('title')
  local handle = nil
  local title = nil
  local calls, ok, err = with_stubbed_system(function()
    handle = T.keymap_gn(dir, false)
    if not handle then return end
    local config = vim.api.nvim_win_get_config(handle.win)
    local chunks = {}
    for _, chunk in ipairs(config.title or {}) do
      chunks[#chunks + 1] = chunk[1]
    end
    title = table.concat(chunks)
    handle.cancel()
  end)
  if handle and vim.api.nvim_win_is_valid(handle.win) then handle.cancel() end
  check(ok, 'the keybinding runs without error: ' .. tostring(err))
  check(title ~= nil and title:find('Session name', 1, true) ~= nil,
    'the title names the popup, got ' .. tostring(title))
  check(title ~= nil and title:find('Esc cancel', 1, true) ~= nil,
    'the title shows the key hint, got ' .. tostring(title))
  check(#calls == 0, 'opening the input runs no command')
end)

test('session name: the prefill helper reads session-meta.json', function()
  local dir = fixture_dir('prefill-read')
  write_file(dir .. '/session-meta.json', '{"name":"my session","description":"desc"}')
  check(T.session_meta_name(dir) == 'my session', 'returns the stored name')

  write_file(dir .. '/session-meta.json', '{"description":"desc"}')
  check(T.session_meta_name(dir) == nil, 'nil when the meta holds no name')

  local missing = dir .. '/missing'
  check(T.session_meta_name(missing) == nil, 'nil when the file is absent')

  local broken = dir .. '/broken'
  vim.fn.mkdir(broken, 'p')
  write_file(broken .. '/session-meta.json', '{"name":')
  check(T.session_meta_name(broken) == nil, 'nil when the file does not parse')

  write_file(broken .. '/session-meta.json', '{"name":42}')
  check(T.session_meta_name(broken) == nil, 'nil when the name is not a string')

  check(T.session_meta_name(nil) == nil, 'nil without a session directory')
end)

test('session name: the handler needs the session info', function()
  local orig_notify = vim.notify
  local orig_exe = M.exe_path
  local orig_session = M.session_id
  local seen = {}
  vim.notify = function(msg) seen[#seen + 1] = msg end
  M.exe_path = nil
  M.session_id = nil
  local handle = T.keymap_gn(tmp_dir, false)
  vim.notify = orig_notify
  M.exe_path = orig_exe
  M.session_id = orig_session
  check(handle == nil, 'no popup without the session info')
  check(seen[1] == 'Session info not available', 'the failure is reported, got ' .. tostring(seen[1]))
end)
