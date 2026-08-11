-- Test harness for tcode.lua. Run headless, e.g.:
--   nvim --headless -l runner.lua <tcode.lua path> [--tmp <dir>] [suite files...]
--
-- Loads the real tcode.lua with test accessors injected IN MEMORY (before the
-- final `return M`), then loads and runs every suite file passed as an arg.
-- The production file itself is never modified.
--
-- Suite files are plain Lua chunks that register tests via the globals
-- provided here: `test(name, fn)`, `check(cond, msg)`, `T` (module internals),
-- `ns` / `thinking_ns_id`, and the helpers `new_buf`, `seed`, `lines_of`,
-- `reset_thinking`, `windowed_render`, `clear_errors`, `recorded_errors`,
-- `tmp_dir`.
--
-- Exit code is 0 only when every assertion passes; the final line always has
-- the shape `TOTAL: <N> passed, <M> failed` so callers can parse it.

local module_path = arg[1]
assert(module_path, 'usage: nvim --headless -l runner.lua <tcode.lua path> [--tmp <dir>] [suite files...]')

-- ---------------------------------------------------------------- module load
local f = assert(io.open(module_path, 'r'), 'cannot open ' .. module_path)
local src = f:read('*all')
f:close()

assert(src:find('return M', 1, true), 'tcode.lua must end with `return M` for the test harness')

local accessors = [[
M.__test = {
  collapse_thinking = collapse_thinking,
  with_modifiable = with_modifiable,
  thinking_state = thinking_state,
  thinking_entries = thinking_entries,
  render_event = render_event,
  toggle_thinking = toggle_thinking,
  toggle_tool_call_args = toggle_tool_call_args,
  create_jsonl_reader = create_jsonl_reader,
  reset_first_event = function() first_event = true end,
}
return M
]]
src = src:gsub('return M%s*$', accessors, 1)
local chunk, load_err = load(src, module_path)
assert(chunk, 'failed to load ' .. module_path .. ': ' .. tostring(load_err))
local M = chunk()
local T = M.__test

-- ------------------------------------------------------------ error recording
-- The display code reports recoverable render/flush errors through
-- nvim_err_writeln. Patch it so suites can assert that nothing was reported.
-- The original is intentionally not restored: the runner process exits right
-- after the report, and recorded errors are echoed to stdout (see the footer)
-- so they stay visible in the test output.
local recorded_errors = {}
vim.api.nvim_err_writeln = function(msg)
  table.insert(recorded_errors, tostring(msg))
end

local function clear_errors()
  -- Clear in place: the patched nvim_err_writeln and the `recorded_errors`
  -- global both reference this same table; rebinding the local would orphan
  -- one of them and make later assertions read stale data.
  for k in pairs(recorded_errors) do
    recorded_errors[k] = nil
  end
end

-- --------------------------------------------------------------- test registry
-- All report output goes through emit(): in `nvim -l` mode, print() writes to
-- stderr, but callers (cargo test / scripts) parse the report from stdout, so
-- write it explicitly to io.stdout for a deterministic stream.
local function emit(line)
  io.stdout:write(line, '\n')
  io.stdout:flush()
end

local suites = {}
local current_suite = nil
local current_test = nil
local passed, failed = 0, 0

local function test(name, fn)
  assert(current_suite, 'test() called outside a suite')
  table.insert(current_suite.tests, { name = name, fn = fn })
end

local function check(cond, msg)
  local prefix = current_test and (current_test.suite .. '.') or ''
  if cond then
    passed = passed + 1
    emit('PASS: ' .. prefix .. msg)
  else
    failed = failed + 1
    emit('FAIL: ' .. prefix .. msg)
  end
end

-- ------------------------------------------------------------------- helpers
local ns = vim.api.nvim_create_namespace('tcode_lua_tests')
local thinking_ns_id = vim.api.nvim_get_namespaces()['tcode_thinking']

local function new_buf()
  -- Scratch buffer, content seeded modifiable then locked read-only (matches
  -- create_display_buffer's modifiable=false invariant).
  local b = vim.api.nvim_create_buf(false, true)
  vim.bo[b].modifiable = false
  return b
end

local function seed(b, content)
  vim.bo[b].modifiable = true
  vim.api.nvim_buf_set_lines(b, 0, -1, false, content)
  vim.bo[b].modifiable = false
end

local function lines_of(b)
  return vim.api.nvim_buf_get_lines(b, 0, -1, false)
end

local function reset_thinking()
  local st = T.thinking_state
  st.is_thinking = false
  st.start_row = nil
  st.content_parts = {}
  st.last_highlighted_row = nil
  st.written = false
end

-- Mirrors create_jsonl_reader's batch window: render_event only ever runs
-- inside a modifiable window in production, so direct calls wrap the same way.
local function windowed_render(b, event, bulk)
  T.with_modifiable(b, function()
    T.render_event(b, ns, event, nil, bulk)
  end)
end

-- ------------------------------------------------------------------- tmp dir
-- `--tmp <dir>`: provided by the Rust test (which owns cleanup). Otherwise a
-- unique dir under <repo>/target/test-tmp/lua-tests/ is created and removed
-- on exit (both success and failure paths).
local tmp_dir = nil
local own_tmp = false
local argv = {}
local i = 2
while i <= #arg do
  if arg[i] == '--tmp' then
    tmp_dir = arg[i + 1]
    i = i + 2
  else
    table.insert(argv, arg[i])
    i = i + 1
  end
end

if not tmp_dir then
  local repo = module_path:match('^(.*)/tcode/lua/tcode%.lua$') or vim.fn.getcwd()
  local root = repo .. '/target/test-tmp/lua-tests'
  vim.fn.mkdir(root, 'p')
  math.randomseed(os.time() + vim.fn.getpid())
  tmp_dir = root .. '/' .. string.format('%08x%08x', math.random(0, 0x7fffffff), math.random(0, 0x7fffffff))
  own_tmp = true
end
-- Create the tmp dir whether it was supplied via --tmp or auto-generated.
vim.fn.mkdir(tmp_dir, 'p')

-- ------------------------------------------------------------------- run all
local function main()
  assert(#argv > 0, 'no suite files given')
  emit('tcode.lua test runner: ' .. module_path .. ' (' .. #argv .. ' suites)')
  emit('tmp dir: ' .. tmp_dir)

  -- Globals used by suite chunks (plain dofile, no require paths).
  _G.test = test
  _G.check = check
  _G.T = T
  _G.M = M
  _G.ns = ns
  _G.thinking_ns_id = thinking_ns_id
  _G.new_buf = new_buf
  _G.seed = seed
  _G.lines_of = lines_of
  _G.reset_thinking = reset_thinking
  _G.windowed_render = windowed_render
  _G.clear_errors = clear_errors
  _G.recorded_errors = recorded_errors
  _G.tmp_dir = tmp_dir

  -- Load suites (registers tests), then run them.
  for _, file in ipairs(argv) do
    local suite = { name = file:match('([^/]+)%.lua$') or file, tests = {} }
    current_suite = suite
    local ok, err = pcall(dofile, file)
    current_suite = nil
    if not ok then
      failed = failed + 1
      emit('ERROR: cannot load suite ' .. file .. ': ' .. tostring(err))
    else
      table.insert(suites, suite)
    end
  end

  for _, suite in ipairs(suites) do
    for _, t in ipairs(suite.tests) do
      current_test = { suite = suite.name, name = t.name }
      local ok, err = pcall(t.fn)
      current_test = nil
      if not ok then
        failed = failed + 1
        emit('ERROR: ' .. suite.name .. '.' .. t.name .. ': ' .. tostring(err))
      end
    end
  end
end

local ok, err = pcall(main)
if own_tmp then
  vim.fn.delete(tmp_dir, 'rf')
end
if #recorded_errors > 0 then
  emit(string.format('recorded nvim_err_writeln errors (%d):', #recorded_errors))
  for _, msg in ipairs(recorded_errors) do
    emit('  ' .. msg)
  end
end
if not ok then
  emit('FATAL: ' .. tostring(err))
  emit(string.format('TOTAL: %d passed, %d failed', passed, failed))
  os.exit(1)
end
emit(string.format('TOTAL: %d passed, %d failed', passed, failed))
os.exit(failed > 0 and 1 or 0)
