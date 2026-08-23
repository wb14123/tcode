-- Pure projection + navigation test suite for project_element / action_at and
-- the integer row-map helpers (element_row / set_element_row / row_element_at).
-- Elements are hand-constructed tables matching the reducer's element shapes;
-- no buffers, no extmarks, no vim.*: the functions under test are pure with
-- respect to element state.

local TC_FENCE = '``````````'

-- Project with an optional width / media root / extra ctx fields (sa_active
-- for the streaming-subagent fence decision).
local function proj(el, width, media_root, ctx_extra)
  local ctx = { width = width, media_root = media_root }
  if ctx_extra then
    for k, v in pairs(ctx_extra) do ctx[k] = v end
  end
  return T.project_element(el, ctx)
end

-- Assert an exact row list with a single check.
local function expect_rows(actual, expected, msg)
  if #actual ~= #expected then
    check(false, msg .. ': row count ' .. #actual .. ' ~= ' .. #expected)
    return
  end
  for i = 1, #expected do
    if actual[i] ~= expected[i] then
      check(false, msg .. ': row ' .. i .. ' = ' .. string.format('%q', actual[i])
        .. ' want ' .. string.format('%q', expected[i]))
      return
    end
  end
  check(true, msg)
end

-- Rebuild the exact label text project_element produces for a tool_call row.
local function tool_label(status, name, created_at, cancel)
  local ts = created_at and os.date('%H:%M:%S', math.floor(created_at / 1000)) or nil
  local l = '► TOOL: [' .. status .. ']'
  if name and name ~= '' then l = l .. ' ' .. name end
  if ts then l = l .. '  ' .. ts end
  if cancel then l = l .. '  [Ctrl-k to cancel]' end
  return l
end

-- Hand-built tool_call with the reducer's field set.
local function tool_call(overrides)
  local el = {
    type = 'tool_call', tool_call_id = 'tc1', tool_name = 'bash',
    tool_call_index = 0, created_at = 1700000000000, args = '',
    args_open = false, args_collapsed = false, output_started = false,
    output_open = false, output = '', output_collapsed = false,
    status = 'generating', error = nil,
  }
  for k, v in pairs(overrides or {}) do el[k] = v end
  return el
end

-- Rebuild the exact label text project_element produces for a subagent row.
local function subagent_label(status, created_at, tokens, desc)
  local ts = created_at and os.date('%H:%M:%S', math.floor(created_at / 1000)) or nil
  local l = '► SUB-AGENT: [' .. status .. ']'
  if ts then l = l .. '  ' .. ts end
  if tokens then l = l .. string.format('  [%d in / %d out]', tokens[1], tokens[2]) end
  if desc and desc ~= '' then l = l .. '  ' .. desc end
  return l
end

-- Hand-built subagent with the reducer's field set.
local function subagent(overrides)
  local el = {
    type = 'subagent', tool_call_id = 'sa1', tool_call_index = 0,
    conversation_id = 'conv1', created_at = 1700000000000,
    description = 'read the file', input = '', input_open = false,
    input_collapsed = false, output = '', output_collapsed = false,
    status = 'running', is_continue = false, error = nil,
  }
  for k, v in pairs(overrides or {}) do el[k] = v end
  return el
end

-- ------------------------------------------------------------------ user/assistant

test('user_message: label + content lines', function()
  local ts = os.date('%H:%M:%S', math.floor(1700000000000 / 1000))
  local el = { type = 'user_message', content = 'hello\nworld', created_at = 1700000000000 }
  expect_rows(proj(el), { '► USER  ' .. ts, 'hello', 'world' }, 'user rows')
end)

test('user_message: empty content -> label only, no trailing blank', function()
  local el = { type = 'user_message', content = '' }
  expect_rows(proj(el), { '► USER' }, 'empty content rows')
end)

test('user_message: no created_at -> no timestamp segment', function()
  local el = { type = 'user_message', content = 'x' }
  expect_rows(proj(el), { '► USER', 'x' }, 'no ts rows')
end)

test('assistant_message: label + content lines', function()
  local ts = os.date('%H:%M:%S', math.floor(1700000000000 / 1000))
  local el = { type = 'assistant_message', content = 'a\nb', created_at = 1700000000000 }
  expect_rows(proj(el), { '► ASSISTANT  ' .. ts, 'a', 'b' }, 'assistant rows')
end)

test('assistant_message: empty content -> label only, no trailing blank', function()
  local el = { type = 'assistant_message', content = '' }
  expect_rows(proj(el), { '► ASSISTANT' }, 'empty content rows')
end)

-- ------------------------------------------------------------------ thinking

test('thinking_block: open streams full content without chrome', function()
  local el = { type = 'thinking_block', state = 'open', content = 'A\nB' }
  expect_rows(proj(el), { 'A', 'B' }, 'open rows')
end)

test('thinking_block: collapsed is a single hint line', function()
  local el = { type = 'thinking_block', state = 'collapsed', content = 'A\nB' }
  expect_rows(proj(el), { '► [Thinking... press o to expand]' }, 'collapsed row')
end)

test('thinking_block: expanded hint above full content', function()
  local el = { type = 'thinking_block', state = 'expanded', content = 'A\nB' }
  expect_rows(proj(el), { '► [Thinking... press o to collapse]', 'A', 'B' }, 'expanded rows')
end)

-- ------------------------------------------------------------------ tool_call

test('tool_call: label with status, name, time, cancel hint (generating)', function()
  local el = tool_call({ status = 'generating' })
  expect_rows(proj(el), {
    tool_label('generating', 'bash', 1700000000000, true),
  }, 'generating label')
end)

test('tool_call: cancel hint for running, absent for permission and done', function()
  local ts = os.date('%H:%M:%S', math.floor(1700000000000 / 1000))
  expect_rows(proj(tool_call({ status = 'running' })), {
    tool_label('running', 'bash', 1700000000000, true),
  }, 'running label')
  expect_rows(proj(tool_call({ status = 'permission' })), {
    '► TOOL: [permission] bash  ' .. ts,
  }, 'permission label')
  expect_rows(proj(tool_call({ status = 'done' })), {
    '► TOOL: [done] bash  ' .. ts,
  }, 'done label')
end)

test('tool_call: no timestamp when created_at is absent', function()
  local el = tool_call({})
  el.created_at = nil
  expect_rows(proj(el), {
    '► TOOL: [generating] bash  [Ctrl-k to cancel]',
  }, 'no ts label')
end)

test('tool_call: args_open streams fence + args, no close fence, no hint', function()
  local el = tool_call({ args_open = true, args = '{"a":1,\n"b":2}' })
  expect_rows(proj(el), {
    tool_label('generating', 'bash', 1700000000000, true),
    '► Param', TC_FENCE, '{"a":1,', '"b":2}',
  }, 'streaming args')
end)

test('tool_call: args_collapsed is ignored — closed args render the full tail', function()
  local el = tool_call({ args = string.rep('x', 100), args_collapsed = true })
  expect_rows(proj(el), {
    tool_label('generating', 'bash', 1700000000000, true),
    '► Param', TC_FENCE, string.rep('x', 100), TC_FENCE,
  }, 'closed args rows')
end)

test('tool_call: multi-line args keep their real rows inside the fence', function()
  local el = tool_call({ args = 'a\nb', args_collapsed = true })
  expect_rows(proj(el), {
    tool_label('generating', 'bash', 1700000000000, true),
    '► Param', TC_FENCE, 'a', 'b', TC_FENCE,
  }, 'multi-line args rows')
end)

test('tool_call: args above the tail cap keep only the last 5 rows', function()
  local lines = {}
  for i = 1, 8 do lines[i] = 'l' .. i end
  local el = tool_call({ args = table.concat(lines, '\n') })
  expect_rows(proj(el), {
    tool_label('generating', 'bash', 1700000000000, true),
    '► Param', TC_FENCE, 'l4', 'l5', 'l6', 'l7', 'l8', TC_FENCE,
  }, 'capped args rows')
end)

test('tool_call: empty args render no Param section', function()
  local el = tool_call({ args = '' })
  expect_rows(proj(el), { tool_label('generating', 'bash', 1700000000000, true) }, 'empty args rows')
end)

test('tool_call: output_open streams after closed args, no close fence', function()
  local el = tool_call({ args = '{}', output_started = true, output_open = true, output = 'out1\nout2', status = 'running' })
  expect_rows(proj(el), {
    tool_label('running', 'bash', 1700000000000, true),
    '► Param', TC_FENCE, '{}', TC_FENCE,
    '► Result', TC_FENCE, 'out1', 'out2',
  }, 'open output')
end)

test('tool_call: output_open with empty output -> fence + one empty line', function()
  local el = tool_call({ args = '{}', output_started = true, output_open = true, output = '', status = 'running' })
  expect_rows(proj(el), {
    tool_label('running', 'bash', 1700000000000, true),
    '► Param', TC_FENCE, '{}', TC_FENCE,
    '► Result', TC_FENCE, '',
  }, 'open empty output')
end)

test('tool_call: output_collapsed is ignored — long output caps at 5 tail lines', function()
  local el = tool_call({ args = '{}', output_started = true,
    output = table.concat({ 'o1', 'o2', 'o3', 'o4', 'o5', 'o6', 'o7', 'o8', 'o9', 'o10' }, '\n'),
    output_collapsed = true, status = 'done' })
  expect_rows(proj(el), {
    tool_label('done', 'bash', 1700000000000),
    '► Param', TC_FENCE, '{}', TC_FENCE,
    '► Result', TC_FENCE, 'o6', 'o7', 'o8', 'o9', 'o10', TC_FENCE,
  }, 'closed output tail')
end)

test('tool_call: streaming output also caps at 5 tail lines', function()
  local el = tool_call({ args = '{}', output_started = true, output_open = true,
    output = table.concat({ 'o1', 'o2', 'o3', 'o4', 'o5', 'o6', 'o7', 'o8', 'o9', 'o10' }, '\n'),
    status = 'running' })
  expect_rows(proj(el), {
    tool_label('running', 'bash', 1700000000000, true),
    '► Param', TC_FENCE, '{}', TC_FENCE,
    '► Result', TC_FENCE, 'o6', 'o7', 'o8', 'o9', 'o10',
  }, 'streaming output tail')
end)

test('tool_call: closed output renders the full tail inside the fence pair', function()
  local el = tool_call({ args = '{}', output_started = true, output = 'o1\no2', status = 'done' })
  expect_rows(proj(el), {
    tool_label('done', 'bash', 1700000000000),
    '► Param', TC_FENCE, '{}', TC_FENCE,
    '► Result', TC_FENCE, 'o1', 'o2', TC_FENCE,
  }, 'closed output rows')
end)

test('tool_call: a single very long line is cut to the last width*5 bytes', function()
  -- 1200-char single line: at width 80 (cap 400) only the last 400 chars show,
  -- so the section never wraps beyond 5 visual rows.
  local el = tool_call({ args = '{}', output_started = true,
    output = string.rep('a', 1200), status = 'done' })
  expect_rows(proj(el, 80), {
    tool_label('done', 'bash', 1700000000000),
    '► Param', TC_FENCE, '{}', TC_FENCE,
    '► Result', TC_FENCE, string.rep('a', 400), TC_FENCE,
  }, 'single long line capped at width*5')
end)

test('tool_call: the char cap tightens at narrow widths', function()
  local el = tool_call({ args = '{}', output_started = true,
    output = string.rep('b', 500), status = 'done' })
  expect_rows(proj(el, 20), {
    tool_label('done', 'bash', 1700000000000),
    '► Param', TC_FENCE, '{}', TC_FENCE,
    '► Result', TC_FENCE, string.rep('b', 100), TC_FENCE,
  }, 'narrow width caps at width*5 chars')
end)

test('tool_call: wide lines are capped by BOTH chars and lines, tail-biased', function()
  -- 10 lines of 100 chars at width 80: 1009 bytes total > 400-byte cap, so the
  -- slice is the last 400 bytes (a mid-line start is acceptable for a tail
  -- view). Assert properties: <= 5 rows, the true last line is the last row,
  -- and the section total is <= width*5 bytes.
  local lines10 = {}
  for i = 1, 10 do lines10[i] = string.rep('a', 100) end
  local el = tool_call({ args = '{}', output_started = true,
    output = table.concat(lines10, '\n'), status = 'done' })
  local rows = proj(el, 80)
  -- Layout: label, '► Param', fence, '{}', fence, '► Result', fence,
  -- <content rows>, close fence — content starts at row 8 (1-indexed).
  local content = {}
  for i = 8, #rows - 1 do content[#content + 1] = rows[i] end
  local total = 0
  for _, l in ipairs(content) do total = total + #l end
  check(#content <= 5, 'wide lines: at most 5 result rows')
  check(content[#content] == string.rep('a', 100), 'wide lines: the true last line is the last row')
  check(total <= 400, 'wide lines: section total <= width*5 bytes')
end)

test('subagent: a long single-line input is also capped by width*5', function()
  local el = subagent({ input = string.rep('c', 600), input_open = false })
  expect_rows(proj(el, 80), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, string.rep('c', 400), TC_FENCE,
    '► Output', TC_FENCE, '', TC_FENCE,
  }, 'long single-line input capped at width*5')
end)

test('tool_call: closed empty output -> one empty row inside the Result fence', function()
  local el = tool_call({ args = '{}', output_started = true, output = '', status = 'done' })
  expect_rows(proj(el), {
    tool_label('done', 'bash', 1700000000000),
    '► Param', TC_FENCE, '{}', TC_FENCE,
    '► Result', TC_FENCE, '', TC_FENCE,
  }, 'closed empty output')
end)

test('tool_call: a trailing newline trims the last empty tail row', function()
  local el = tool_call({ output_started = true, output = 'o1\n', status = 'done' })
  expect_rows(proj(el), {
    tool_label('done', 'bash', 1700000000000),
    '► Result', TC_FENCE, 'o1', TC_FENCE,
  }, 'trimmed tail')
end)

test('tool_call: full_input (detail view) renders the full content, no cap', function()
  -- The detail view sets model.full_input: sections show the complete content,
  -- never the 5-line display tail.
  local el = tool_call({ full_input = true, output_started = true, status = 'done',
    output = table.concat({ 'o1', 'o2', 'o3', 'o4', 'o5', 'o6', 'o7' }, '\n') })
  expect_rows(proj(el), {
    tool_label('done', 'bash', 1700000000000),
    '► Result', TC_FENCE, 'o1', 'o2', 'o3', 'o4', 'o5', 'o6', 'o7', TC_FENCE,
  }, 'full_input full rows')
end)

-- -------------------------------------------------- NUL + UTF-8 boundary fixes

test('tool_call: NUL bytes in args/output project as the escaped \\0 display', function()
  -- A NUL byte is an internal line break to nvim's buffer API; the projector
  -- escapes it (\0 -> backslash-zero) so a projected row stays one buffer
  -- row. The model keeps the raw byte (covered in regression_tests).
  local el = tool_call({ args = 'a\0b\nc', output_started = true, output = 'o\0ut', status = 'done' })
  expect_rows(proj(el), {
    tool_label('done', 'bash', 1700000000000),
    '► Param', TC_FENCE, 'a\\0b', 'c', TC_FENCE,
    '► Result', TC_FENCE, 'o\\0ut', TC_FENCE,
  }, 'escaped NUL rows')
end)

test('subagent: NUL bytes in input project as the escaped \\0 display', function()
  local el = subagent({ input = 'in\0put' })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, 'in\\0put', TC_FENCE,
    '► Output', TC_FENCE, '', TC_FENCE,
  }, 'escaped NUL input rows')
end)

test('tool_call: a byte-sliced tail starts on a valid UTF-8 boundary', function()
  -- tail_capped slices to the last width*5 BYTES; when that cuts a multi-byte
  -- character, the leading continuation bytes (0x80-0xBF) are dropped so the
  -- first displayed byte is a complete character lead.
  -- 'éééé' is 8 bytes; width 1 caps at 5 bytes: the slice starts with the
  -- continuation byte of the second é, which must be dropped -> 'éé'.
  local el = tool_call({ output_started = true, output = 'éééé', status = 'done' })
  local rows = proj(el, 1)
  local content_row = rows[#rows - 1] -- last content row before the close fence
  check(content_row == 'éé', 'tail starts on a complete character (éé of éééé)')
  local b = content_row:byte(1)
  check(b ~= nil and (b < 0x80 or b > 0xBF), 'first byte 0x' .. string.format('%02X', b)
    .. ' is not a UTF-8 continuation byte')
  -- Multi-byte cut at the START of a multi-line slice: the leading
  -- continuation byte is dropped and the line split still lands correctly.
  -- 'xxxxxééxxxx\nlast' is 18 bytes; width 2 caps at 10: the slice starts
  -- with the continuation byte of the second é, which must be dropped.
  local el2 = tool_call({ output_started = true,
    output = 'xxxxx' .. 'éé' .. 'xxxx' .. '\nlast', status = 'done' })
  local rows2 = proj(el2, 2)
  check(rows2[#rows2 - 2] == 'xxxx' and rows2[#rows2 - 1] == 'last',
    'multi-line tail: leading continuation byte dropped, lines intact')
end)

-- ------------------------------------------------------------------ subagent

test('subagent: label with status, time, tokens, desc; empty sections', function()
  local el = subagent({ input_tokens = 12, output_tokens = 34 })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, { 12, 34 }, 'read the file'),
    '► Output', TC_FENCE, '', TC_FENCE,
  }, 'label + empty sections')
end)

test('subagent: no tokens segment when counters are unset', function()
  local el = subagent()
  check(proj(el)[1] == subagent_label('running', 1700000000000, nil, 'read the file'), 'no tokens segment')
end)

test('subagent: continue flag renders continuing status', function()
  local el = subagent({ is_continue = true, status = 'continuing' })
  check(proj(el)[1] == subagent_label('continuing', 1700000000000, nil, 'read the file'), 'continuing label')
end)

test('subagent: unknown multiline status and desc flattened via single_line', function()
  local el = subagent({ status = 'odd\nstatus', description = 'd1\nd2' })
  check(proj(el)[1] == subagent_label('odd status', 1700000000000, nil, 'd1 d2'), 'single_line on status and desc')
end)

test('subagent: input_open streams fenced input, no output section', function()
  local el = subagent({ input_open = true, input = 'in1\nin2' })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, 'in1', 'in2',
  }, 'open input')
end)

test('subagent: input_collapsed is ignored — closed input renders the full tail', function()
  local el = subagent({ input = string.rep('z', 100), input_collapsed = true })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, string.rep('z', 100), TC_FENCE,
    '► Output', TC_FENCE, '', TC_FENCE,
  }, 'closed input rows')
end)

test('subagent: closed input renders header + fence pair + content rows', function()
  local el = subagent({ input = 'i1\ni2' })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, 'i1', 'i2', TC_FENCE,
    '► Output', TC_FENCE, '', TC_FENCE,
  }, 'closed input rows')
end)

test('subagent: output_collapsed is ignored — long output caps at 5 tail lines', function()
  local el = subagent({ input = '{}',
    output = table.concat({ 'w1', 'w2', 'w3', 'w4', 'w5', 'w6', 'w7', 'w8' }, '\n'),
    output_collapsed = true })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, '{}', TC_FENCE,
    '► Output', TC_FENCE, 'w4', 'w5', 'w6', 'w7', 'w8', TC_FENCE,
  }, 'closed output tail')
end)

test('subagent: closed output renders header + fence pair + content rows', function()
  local el = subagent({ input = '{}', output = 'o1\no2' })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, '{}', TC_FENCE,
    '► Output', TC_FENCE, 'o1', 'o2', TC_FENCE,
  }, 'closed output rows')
end)

test('subagent: empty output -> one empty row inside the Output fence', function()
  local el = subagent({ input = '{}' })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, '{}', TC_FENCE,
    '► Output', TC_FENCE, '', TC_FENCE,
  }, 'empty output row')
end)

test('subagent: error rows after a blank line inside the Output fence', function()
  local el = subagent({ input = '{}', output = 'o1', error = 'boom' })
  expect_rows(proj(el), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, '{}', TC_FENCE,
    '► Output', TC_FENCE, 'o1', '', 'Error: boom', TC_FENCE,
  }, 'error rows')
end)

test('subagent: streaming output renders without the close fence', function()
  local el = subagent({ input = '{}', output = 'o1\no2' })
  expect_rows(proj(el, nil, nil, { sa_active = 'conv1' }), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, '{}', TC_FENCE,
    '► Output', TC_FENCE, 'o1', 'o2',
  }, 'streaming output rows')
end)

test('subagent: streaming output of a different conversation keeps the close fence', function()
  local el = subagent({ input = '{}', output = 'o1' })
  expect_rows(proj(el, nil, nil, { sa_active = 'other-conv' }), {
    subagent_label('running', 1700000000000, nil, 'read the file'),
    '► Input', TC_FENCE, '{}', TC_FENCE,
    '► Output', TC_FENCE, 'o1', TC_FENCE,
  }, 'non-active conversation rows')
end)

-- ------------------------------------------------------------------ system/media/retry

test('system_message: level in the label + message lines', function()
  local el = { type = 'system_message', level = 'Warning', message = 'disk\nfull' }
  expect_rows(proj(el), { '► SYSTEM [Warning]', 'disk', 'full' }, 'system rows')
end)

test('system_message: unknown level falls back to Info', function()
  local el = { type = 'system_message', message = 'm' }
  expect_rows(proj(el), { '► SYSTEM [Info]', 'm' }, 'default level')
end)

test('media: blank + img line when a media root is available', function()
  local el = { type = 'media', relative_path = 'pic.png' }
  expect_rows(proj(el, 80, 'file:///sessions/s1/media/'), {
    '', '![img](file://file:///sessions/s1/media/pic.png)',
  }, 'media rows')
end)

test('media: concatenates the precomputed encoded media root', function()
  local root = vim.uri_encode('/home/u/.tcode/sessions/s1/media/')
  local el = { type = 'media', relative_path = 'pic with space.png' }
  expect_rows(proj(el, 80, root), { '', '![img](file://' .. root .. 'pic with space.png)' }, 'encoded root')
end)

test('media: zero rows without media_root or with an empty path', function()
  expect_rows(proj({ type = 'media', relative_path = 'p.png' }), {}, 'no media root')
  expect_rows(proj({ type = 'media', relative_path = '' }, 80, 'file:///root/'), {}, 'empty path')
  expect_rows(proj({ type = 'media' }, 80, 'file:///root/'), {}, 'nil path')
end)

test('retry: chrome label + extra reason lines', function()
  local el = { type = 'retry', attempt = 1, max_retries = 3, reason = 'rate limited\nmore detail' }
  expect_rows(proj(el), { '► [Retrying... (attempt 1/3) -- rate limited]', 'more detail' }, 'retry rows')
end)

-- ------------------------------------------------------------------ end_info / end_marker

test('end_info: token line without cache read', function()
  local el = { type = 'end_info', tokens = { input_tokens = 12, output_tokens = 4 } }
  expect_rows(proj(el), { '► [12 in / 4 out tokens]' }, 'tokens only')
end)

test('end_info: cache read segment shown when non-zero', function()
  local el = { type = 'end_info', tokens = { input_tokens = 12, cache_read_input_tokens = 5, output_tokens = 4 } }
  expect_rows(proj(el), { '► [12 in / 5 cache read / 4 out tokens]' }, 'cache read')
end)

test('end_info: cache creation folded into the input count', function()
  local el = { type = 'end_info', tokens = { input_tokens = 10, cache_creation_input_tokens = 2, output_tokens = 3 } }
  expect_rows(proj(el), { '► [12 in / 3 out tokens]' }, 'creation folded')
end)

test('end_info: TOOL prefix on the token line', function()
  local el = { type = 'end_info', token_prefix = 'TOOL', tokens = { input_tokens = 12, output_tokens = 4 } }
  expect_rows(proj(el), { '► [TOOL: 12 in / 4 out tokens]' }, 'TOOL prefix')
end)

test('end_info: TOOL zero tokens -> no token line, status-only row', function()
  local el = { type = 'end_info', token_prefix = 'TOOL', tokens = { input_tokens = 0, output_tokens = 0 }, end_status = 'Failed' }
  expect_rows(proj(el), { '► [Failed]' }, 'status only')
end)

test('end_info: status suffix appended to the token line', function()
  local el = { type = 'end_info', tokens = { input_tokens = 12, output_tokens = 4 }, end_status = 'Failed' }
  expect_rows(proj(el), { '► [12 in / 4 out tokens] [Failed]' }, 'status suffix')
end)

test('end_info: Succeeded status -> no suffix', function()
  local el = { type = 'end_info', tokens = { input_tokens = 12, output_tokens = 4 }, end_status = 'Succeeded' }
  expect_rows(proj(el), { '► [12 in / 4 out tokens]' }, 'no suffix')
end)

test('end_info: zero rows when nothing to show', function()
  expect_rows(proj({ type = 'end_info', tokens = {}, end_status = 'Succeeded' }), {}, 'empty tokens')
  expect_rows(proj({ type = 'end_info' }), {}, 'nil fields')
end)

test('end_info: error lines only when no tokens or status', function()
  local el = { type = 'end_info', error = 'boom' }
  expect_rows(proj(el), { 'Error: boom' }, 'error only')
end)

test('end_info: token line followed by error lines', function()
  local el = { type = 'end_info', tokens = { input_tokens = 1, output_tokens = 2 }, error = 'boom\nsecond' }
  expect_rows(proj(el), { '► [1 in / 2 out tokens]', 'Error: boom', 'second' }, 'tokens + error')
end)

test('end_info: multiline status flattened via single_line', function()
  local el = { type = 'end_info', tokens = { input_tokens = 1, output_tokens = 2 }, end_status = 'Bad\nStatus' }
  expect_rows(proj(el), { '► [1 in / 2 out tokens] [Bad Status]' }, 'flattened status')
end)

test('end_marker: total line with cache read segment', function()
  local el = { type = 'end_marker', tokens = {
    total_input_tokens = 10, total_cache_creation_tokens = 5,
    total_cache_read_tokens = 5, total_output_tokens = 7,
  } }
  expect_rows(proj(el), { '► [Total: 15 in / 5 cache read / 7 out tokens]' }, 'with cache read')
end)

test('end_marker: cache read segment omitted when zero', function()
  local el = { type = 'end_marker', tokens = {
    total_input_tokens = 10, total_cache_creation_tokens = 5,
    total_cache_read_tokens = 0, total_output_tokens = 7,
  } }
  expect_rows(proj(el), { '► [Total: 15 in / 7 out tokens]' }, 'without cache read')
end)

-- ------------------------------------------------------------------ action_at

test('action_at: thinking states', function()
  local collapsed = { type = 'thinking_block', state = 'collapsed', content = 'x' }
  check(T.action_at(collapsed, 0) == 'thinking', 'collapsed row toggles')
  check(T.action_at(collapsed, 1) == nil, 'beyond collapsed row is nil')
  local expanded = { type = 'thinking_block', state = 'expanded', content = 'a\nb' }
  check(T.action_at(expanded, 0) == 'thinking', 'expanded hint toggles')
  check(T.action_at(expanded, 1) == 'thinking', 'expanded content toggles')
  check(T.action_at(expanded, 2) == 'thinking', 'expanded content row 2 toggles')
  local open = { type = 'thinking_block', state = 'open', content = 'a' }
  check(T.action_at(open, 0) == nil, 'open thinking is not navigable')
end)

test('action_at: tool_call rows are all detail', function()
  local el = tool_call({ args = 'x\ny', output_started = true, output = 'o1' })
  -- rows: label, Param, fence, x, y, fence, Result, fence, o1, fence
  for off = 0, 9 do
    check(T.action_at(el, off) == 'detail', ('row %d -> detail'):format(off))
  end
end)

test('action_at: streaming tool rows are still detail', function()
  local el = tool_call({ args = 'a1', args_open = true, output_started = true, output_open = true, output = 'o1' })
  -- rows: label, Param, fence, a1, Result, fence, o1
  for off = 0, 6 do
    check(T.action_at(el, off) == 'detail', ('streaming row %d -> detail'):format(off))
  end
end)

test('action_at: subagent rows are all detail', function()
  local el = subagent({ input = 'i1', output = 'o1' })
  -- rows: label, Input, fence, i1, fence, Output, fence, o1, fence
  for off = 0, 8 do
    check(T.action_at(el, off) == 'detail', ('subagent row %d -> detail'):format(off))
  end
end)

test('action_at: streaming subagent rows are still detail', function()
  local el = subagent({ input = 'i1\ni2', input_open = true })
  -- rows: label, Input, fence, i1, i2
  for off = 0, 4 do
    check(T.action_at(el, off) == 'detail', ('streaming input row %d -> detail'):format(off))
  end
end)

test('action_at: nil for non-interactive element types', function()
  check(T.action_at({ type = 'user_message', content = 'x' }, 0) == nil, 'user nil')
  check(T.action_at({ type = 'assistant_message', content = 'x' }, 1) == nil, 'assistant nil')
  check(T.action_at({ type = 'system_message', level = 'Info', message = 'm' }, 0) == nil, 'system nil')
  check(T.action_at({ type = 'media', relative_path = 'p' }, 0) == nil, 'media nil')
  check(T.action_at({ type = 'retry', attempt = 1, max_retries = 2, reason = 'r' }, 0) == nil, 'retry nil')
  check(T.action_at({ type = 'end_info', tokens = { input_tokens = 1, output_tokens = 1 } }, 0) == nil, 'end_info nil')
  check(T.action_at({ type = 'end_marker', tokens = {} }, 0) == nil, 'end_marker nil')
  check(T.action_at(nil, 0) == nil, 'nil element nil')
end)

-- ------------------------------------------------------------------ chrome spans

test('element_chrome_spans: tool label parts (generating)', function()
  local spans = T.element_chrome_spans(tool_call({ status = 'generating' }), {})
  local want = {
    { row = 0, start_col = 0, end_col = 9, group = 'TCodeTool' },
    { row = 0, start_col = 9, end_col = 22, group = 'TCodeTool' },
    { row = 0, start_col = 22, end_col = 27, group = 'TCodeTool' },
    { row = 0, start_col = 27, end_col = 37, group = 'TCodeTokens' },
    { row = 0, start_col = 37, end_col = 57, group = 'TCodeTokens' },
  }
  check(#spans == #want, 'generating label span count')
  for i = 1, #want do
    local a, b = spans[i], want[i]
    check(a.row == b.row and a.start_col == b.start_col and a.end_col == b.end_col and a.group == b.group,
      ('generating span %d'):format(i - 1))
  end
end)

test('element_chrome_spans: status group varies per status', function()
  local function status_group(status)
    local spans = T.element_chrome_spans(tool_call({ status = status }), {})
    return spans[2].group, spans[2].start_col, spans[2].end_col
  end
  local g, s, e = status_group('running')
  check(g == 'TCodeTool' and s == 9 and e == 19, 'running uses TCodeTool')
  g, s, e = status_group('permission')
  check(g == 'TCodePermission' and s == 9 and e == 22, 'permission uses TCodePermission')
  g, s, e = status_group('done')
  check(g == 'TCodeSuccess' and s == 9 and e == 16, 'done uses TCodeSuccess')
  g, s, e = status_group('failed')
  check(g == 'TCodeError' and s == 9 and e == 18, 'failed uses TCodeError')
end)

test('element_chrome_spans: subagent label with tokens and desc', function()
  local spans = T.element_chrome_spans(subagent({ input_tokens = 12, output_tokens = 34 }), {})
  -- Filter to the label row: the Output header row carries its own span.
  local label_spans = {}
  for _, sp in ipairs(spans) do
    if sp.row == 0 then label_spans[#label_spans + 1] = sp end
  end
  local want = {
    { row = 0, start_col = 0, end_col = 14, group = 'TCodeTool' },
    { row = 0, start_col = 14, end_col = 24, group = 'TCodeTool' },
    { row = 0, start_col = 24, end_col = 34, group = 'TCodeTokens' },
    { row = 0, start_col = 34, end_col = 52, group = 'TCodeTokens' },
    { row = 0, start_col = 52, end_col = 67, group = 'TCodeTool' },
  }
  check(#label_spans == #want, 'subagent label span count')
  for i = 1, #want do
    local a, b = label_spans[i], want[i]
    check(a.row == b.row and a.start_col == b.start_col and a.end_col == b.end_col and a.group == b.group,
      ('subagent span %d'):format(i - 1))
  end
end)

test('element_chrome_spans: subagent status groups', function()
  local function status_group(status)
    local spans = T.element_chrome_spans(subagent({ status = status }), {})
    return spans[2].group
  end
  check(status_group('running') == 'TCodeTool', 'running -> TCodeTool')
  check(status_group('continuing') == 'TCodeTool', 'continuing -> TCodeTool')
  check(status_group('permission') == 'TCodePermission', 'permission -> TCodePermission')
  check(status_group('turn ended') == 'TCodeTokens', 'turn ended -> TCodeTokens')
  check(status_group('done') == 'TCodeSuccess', 'done -> TCodeSuccess')
  check(status_group('odd status') == 'TCodeError', 'unknown status -> TCodeError')
end)

test('element_layout: user and assistant label spans with ts', function()
  local u = T.element_layout({ type = 'user_message', content = 'hi', created_at = 1700000000000 }, {})
  check(u[1].text == '► USER  17:13:20', 'user label text')
  -- Layout row spans are positional { start_col, end_col, group }.
  check(u[1].spans[1][1] == 0 and u[1].spans[1][2] == 8 and u[1].spans[1][3] == 'TCodeUser',
    'user label part')
  check(u[1].spans[2][1] == 8 and u[1].spans[2][2] == 18 and u[1].spans[2][3] == 'TCodeTokens',
    'user ts part')
  check(u[2].text == 'hi' and u[2].spans == nil, 'content rows carry no chrome spans')
  local a = T.element_layout({ type = 'assistant_message', content = 'hi', created_at = 1700000000000 }, {})
  check(a[1].text == '► ASSISTANT  17:13:20', 'assistant label text')
  check(a[1].spans[1][1] == 0 and a[1].spans[1][2] == 13 and a[1].spans[1][3] == 'TCodeAssistant',
    'assistant label part')
end)

test('element_layout: section headers carry TCodeTokens col ranges', function()
  local el = tool_call({ args = '{}', output_started = true, output = 'o', status = 'done' })
  local layout = T.element_layout(el, {})
  check(layout[2].text == '► Param' and layout[2].spans[1][1] == 0
    and layout[2].spans[1][2] == 9 and layout[2].spans[1][3] == 'TCodeTokens', 'Param header span')
  check(layout[6].text == '► Result' and layout[6].spans[1][1] == 0
    and layout[6].spans[1][2] == 10 and layout[6].spans[1][3] == 'TCodeTokens', 'Result header span')
  check(layout[3].spans == nil and layout[4].spans == nil, 'fence + content rows carry no chrome spans')
  local sa = T.element_layout(subagent({ input = '{}', output = 'o' }), {})
  check(sa[2].text == '► Input' and sa[2].spans[1][2] == 9, 'Input header span')
  check(sa[6].text == '► Output' and sa[6].spans[1][2] == 10, 'Output header span')
end)

test('element_chrome_spans: end_info token line with status suffix', function()
  local el = { type = 'end_info', tokens = { input_tokens = 12, output_tokens = 4 }, end_status = 'Failed' }
  local spans = T.element_chrome_spans(el, {})
  check(#spans == 2, 'two parts on the token line')
  check(spans[1].row == 0 and spans[1].start_col == 0 and spans[1].end_col == 26 and spans[1].group == 'TCodeTokens',
    'token part')
  check(spans[2].row == 0 and spans[2].start_col == 26 and spans[2].end_col == 35 and spans[2].group == 'TCodeError',
    'status suffix part')
end)

test('element_layout: project_element text matches the layout rows', function()
  local el = tool_call({ args = '{}', output_started = true, output = 'o', status = 'done' })
  local layout = T.element_layout(el, {})
  local rows = {}
  for _, row in ipairs(layout) do rows[#rows + 1] = row.text end
  expect_rows(T.project_element(el, {}), rows, 'project_element matches layout texts')
end)

-- ------------------------------------------------------------------ row map

test('get_renderer_state: rows field present and fresh per model', function()
  local m = T.reset_model()
  local st = T.get_renderer_state(m)
  check(type(st.rows) == 'table', 'rows field exists')
  check(next(st.rows) == nil, 'rows field starts empty')
end)

test('row_element_at: tuple return, zero-height skip, end-first scan', function()
  local m = T.reset_model()
  local e1 = { type = 'user_message', content = 'a', id = 1 }
  local e2 = { type = 'system_message', level = 'Info', message = 'b', id = 2 }
  local e0 = { type = 'end_info', id = 3 } -- zero-height: no start_row
  m.elements = { e1, e2, e0 }
  m.by_id = { [1] = e1, [2] = e2, [3] = e0 }
  local st = T.get_renderer_state(m)
  T.set_element_row(st, e1, 0, 2)
  T.set_element_row(st, e2, 2, 1)
  T.set_element_row(st, e0, nil, 0)

  local el, offset = T.row_element_at(m, 0)
  check(el == e1 and offset == 0, 'row 0 -> e1 offset 0')
  el, offset = T.row_element_at(m, 1)
  check(el == e1 and offset == 1, 'row 1 -> e1 offset 1')
  el, offset = T.row_element_at(m, 2)
  check(el == e2 and offset == 0, 'row 2 -> e2 offset 0 (zero-height skipped)')
  el, offset = T.row_element_at(m, 3)
  check(el == nil and offset == nil, 'row 3 -> nil')

  local entry = T.element_row(st, e1)
  check(entry and entry.start_row == 0 and entry.height == 2, 'element_row reads the entry')
  check(T.element_row(st, e0).start_row == nil, 'zero-height entry has no start_row')
  check(T.element_row(st, { id = 999 }) == nil, 'unknown element -> nil entry')
end)
