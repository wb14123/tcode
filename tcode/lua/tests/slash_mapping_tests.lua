-- Edit-window insert-mode handlers: keymap_slash ('/') and keymap_tab (<Tab>).
--
-- Both feed their literal key back with nvim_feedkeys. That call must carry the
-- 'i' flag: without it feedkeys APPENDS to the typeahead buffer, so when input
-- arrives as one burst (a paste delivered as raw keys, fast typing, key repeat)
-- the rest of the burst is still queued and the literal key is inserted only
-- after it. Pasting a URL then moves every slash to the end of the text:
--     https://example.com/a/b/c  ->  https:example.comabc/////

-- Swap vim.schedule for a collector while `fn` runs, so the popup branch's
-- deferred <C-x><C-u> is asserted instead of leaking into a later suite.
-- Restores vim.schedule on both success and error.
local function with_captured_schedule(fn)
  local orig = vim.schedule
  local captured = {}
  vim.schedule = function(cb) captured[#captured + 1] = cb end
  local ok, err = pcall(fn)
  vim.schedule = orig
  if not ok then error(err, 0) end
  return captured
end

-- Feed `keys` as one burst into a fresh buffer with `handler` mapped to `name`.
-- Returns the resulting lines, how many times the handler ran, and how many
-- callbacks it scheduled. Restores the previous current buffer and removes the
-- keymap when done.
local function burst(name, handler, keys, setup)
  local b = new_buf()
  seed(b, { '' })
  vim.bo[b].modifiable = true
  if setup then setup(b) end

  local calls = 0
  vim.keymap.set('i', name, function()
    calls = calls + 1
    handler()
  end, { buffer = b })

  local prev = vim.api.nvim_get_current_buf()
  vim.api.nvim_set_current_buf(b)
  local captured = with_captured_schedule(function()
    vim.api.nvim_feedkeys(keys, 'x', false)
  end)
  vim.cmd('stopinsert')

  local lines = lines_of(b)
  vim.keymap.del('i', name, { buffer = b })
  pcall(vim.api.nvim_set_current_buf, prev)
  return { lines = lines, calls = calls, scheduled = #captured }
end

test('slash mapping: URL pasted as one burst keeps every slash in place', function()
  -- One burst: the whole URL is still in typeahead when the '/' mapping fires
  -- for the first slash, so an appended key would land after the whole URL.
  local r = burst('/', T.keymap_slash, 'ihttps://example.com/a/b/c')
  check(r.lines[1] == 'https://example.com/a/b/c',
    'URL inserted in order, slashes not relocated to the end')
  check(r.calls == 5, 'handler ran once per slash (5)')
  check(r.scheduled == 0, 'no popup for slashes inside a URL')
end)

test('slash mapping: burst starting with a slash keeps it at the front', function()
  -- Leading '/' takes the popup branch, whose literal insert must also land at
  -- the front of the queued burst.
  local r = burst('/', T.keymap_slash, 'i/usr/local/bin')
  check(r.lines[1] == '/usr/local/bin', 'leading slash stays at the front')
  check(#r.lines == 1, 'burst stays on one line')
  check(r.calls == 3, 'handler ran once per slash (3)')
  check(r.scheduled == 1, 'completion popup scheduled for the leading slash')
end)

test('slash mapping: a single typed slash is inserted exactly once', function()
  local r = burst('/', T.keymap_slash, 'i/')
  check(r.lines[1] == '/', 'one slash typed yields one slash in the buffer')
  check(r.calls == 1, 'handler ran once')
  check(r.scheduled == 1, 'completion popup scheduled for the leading slash')
end)

test('tab mapping: burst with tabs keeps them in place', function()
  local r = burst('<Tab>', function() T.keymap_tab({}) end, 'ia\tb\tc', function(b)
    -- Pin the options that decide whether <Tab> inserts a literal tab.
    vim.bo[b].expandtab = false
    vim.bo[b].softtabstop = 0
  end)
  check(r.lines[1] == 'a\tb\tc', 'tabs not relocated to the end of the burst')
  check(r.calls == 2, 'handler ran once per tab (2)')
end)
