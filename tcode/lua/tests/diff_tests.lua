-- Unit tests for the edit-diff core of tcode.lua: the streaming-tolerant args
-- scanner, the hunk row builder, and the capped window. Pure helpers only — no
-- buffers, no extmarks, no rendering; every case drives `T.<helper>` with raw
-- bytes and asserts the returned tables. The args text is always the raw JSON
-- as it arrives on the wire, so the fixtures below are byte-exact (escaped
-- backslash sequences, no decoded newlines in the input).

-- The worked example args: one JSON line, values escaped with \n.
local ARGS = '{"file_path":"/repo/src/a.rs",'
  .. '"old_string":"fn a() {\\n    one();\\n}\\n",'
  .. '"new_string":"fn a() {\\n    two();\\n}\\n",'
  .. '"replace_all":false}'

-- Compact args used by the truncation cuts.
local CUT_ARGS = '{"file_path":"/a.rs","old_string":"x\\ny","new_string":"z"}'

-- Malformed wire values: an invalid \u escape and a boolean where a string
-- belongs. Neither may raise or freeze the element.
local MALFORMED_ARG = '{"old_string":"a\\u-1f0b","new_string":"c"}'
local BOOLEAN_ARG = '{"old_string":true,"new_string":"b"}'

local function kinds(rows)
  local out = {}
  for _, r in ipairs(rows) do out[#out + 1] = r.kind end
  return table.concat(out, ',')
end

-- ------------------------------------------------------------ args scanning

test('scan: complete args expose every diff field', function()
  local s = T.scan_edit_args(ARGS)
  check(s.file_path == '/repo/src/a.rs', 'file_path decoded')
  check(s.old_string == 'fn a() {\n    one();\n}\n', 'old_string decoded to real newlines')
  check(s.new_string == 'fn a() {\n    two();\n}\n', 'new_string decoded to real newlines')
  check(s.replace_all == false, 'replace_all false')
  local t = T.scan_edit_args('{"old_string":"a","new_string":"b","replace_all":true}')
  check(t.replace_all == true, 'replace_all true')
  local u = T.scan_edit_args('{"file_path":"/a","extra":{"k":[1,2,{"z":null}]},"old_string":"x"}')
  check(u.file_path == '/a' and u.old_string == 'x', 'unknown keys and nested containers ignored')
  check(u.new_string == nil, 'new_string nil while its key is absent')
  local empty = T.scan_edit_args('')
  check(empty.file_path == nil and empty.old_string == nil, 'empty args scan to nothing')
end)

test('scan: escape decoding is byte exact', function()
  local s = T.scan_edit_args('{"old_string":"q\\"q\\\\q\\/q\\b\\f\\n\\r\\t\\u4e2d\\ud83d\\ude00"}')
  local plain = 'q"q\\q/q\b\f\n\r\t'
  local expected = plain .. '\228\184\173' .. '\240\159\152\128'
  check(s.old_string == expected, 'every escape form decodes to its bytes')
  check(#s.old_string == #expected, 'no bytes added or dropped')
  check(s.old_string:find('\228\184\173', 1, true) == #plain + 1, 'BMP \\uXXXX becomes 3-byte UTF-8')
  check(s.old_string:find('\\u4e2d', 1, true) == nil, 'the escape text itself is gone')
  check(s.old_string:sub(-4) == '\240\159\152\128', 'surrogate pair becomes one 4-byte character')
end)

test('scan: truncated args yield the decoded prefix and never raise', function()
  local partial = T.scan_edit_args('{"file_path":"/rep')
  check(partial.file_path == '/rep' and partial.old_string == nil, 'mid-value cut: decoded prefix, later keys nil')

  local backslash = T.scan_edit_args('{"old_string":"ab\\')
  check(backslash.old_string == 'ab', 'cut right after \\ : partial escape dropped')

  local unicode = T.scan_edit_args('{"old_string":"a\\u12')
  check(unicode.old_string == 'a', 'cut inside \\uXXXX: partial escape dropped')

  local mid_key = T.scan_edit_args('{"file_p')
  check(mid_key.file_path == nil, 'cut mid-key yields no field')

  local no_brace = T.scan_edit_args(CUT_ARGS:sub(1, #CUT_ARGS - 1))
  check(no_brace.new_string == 'z', 'the outer } is not needed once the values are decoded')

  local raised = 0
  for _, fixture in ipairs({ CUT_ARGS, MALFORMED_ARG, BOOLEAN_ARG }) do
    for cut = 1, #fixture do
      if not pcall(T.scan_edit_args, fixture:sub(1, cut)) then raised = raised + 1 end
    end
  end
  check(raised == 0, 'every prefix of every fixture scans without error')
end)

test('scan: a malformed \\u escape keeps its bytes and never raises', function()
  local signed = T.scan_edit_args('{"old_string":"a\\u-1f0b"}')
  check(signed.old_string == 'a\\u-1f0b', 'a signed escape is not read as hex')
  local spaced = T.scan_edit_args('{"old_string":"a\\u 123b"}')
  check(spaced.old_string == 'a\\u 123b', 'a leading space is not read as hex')
  local prefixed = T.scan_edit_args('{"old_string":"a\\u0x12b"}')
  check(prefixed.old_string == 'a\\u0x12b', 'a 0x prefix is not read as hex')
end)

test('scan: a non-string value reads as absent', function()
  local b = T.scan_edit_args(BOOLEAN_ARG)
  check(b.old_string == nil and b.new_string == 'b', 'a boolean old_string is ignored, its neighbour decodes')
  local p = T.scan_edit_args('{"file_path":true,"old_string":"x"}')
  check(p.file_path == nil and p.old_string == 'x', 'a boolean file_path is ignored')
  local n = T.scan_edit_args('{"old_string":null,"new_string":"x"}')
  check(n.old_string == nil and n.new_string == 'x', 'a null value is ignored')
  local r = T.scan_edit_args('{"old_string":"x","replace_all":true}')
  check(r.replace_all == true, 'replace_all still decodes as a boolean')
end)

test('scan: a key name inside old_string is content, not a key', function()
  local s = T.scan_edit_args(
    '{"old_string":"before \\"new_string\\": \\"x\\" after","new_string":"changed"}')
  check(s.old_string == 'before "new_string": "x" after', 'quoted key text stays in old_string')
  check(s.new_string == 'changed', 'the real new_string key supplies the value')
  local nested = T.scan_edit_args('{"meta":{"new_string":"nope"},"new_string":"yes"}')
  check(nested.new_string == 'yes', 'a nested key cannot win over the top-level one')
end)

-- ---------------------------------------------------------------- hunk rows

test('hunk: one changed line in a 5-line block', function()
  local rows = T.hunk_rows('a\nb\nc\nd\ne', 'a\nb\nX\nd\ne')
  check(kinds(rows) == 'context,context,del,add,context,context', 'row kinds and order')
  check(rows[1].text == 'a' and rows[2].text == 'b', 'leading context is the trimmed prefix')
  check(rows[3].text == 'c' and rows[4].text == 'X', 'removed then added line')
  check(rows[5].text == 'd' and rows[6].text == 'e', 'trailing context is the trimmed suffix')
end)

test('hunk: context is capped at 2 rows per side, trimming is exact', function()
  local rows = T.hunk_rows('l1\nl2\nl3\nl4\nl5\nl6\nl7', 'l1\nl2\nl3\nL4\nl5\nl6\nl7')
  check(#rows == 6, 'a long common region does not grow the hunk')
  check(rows[1].text == 'l2' and rows[2].text == 'l3', 'only the last 2 prefix lines survive')
  check(rows[3].kind == 'del' and rows[3].text == 'l4', 'del at the change')
  check(rows[4].kind == 'add' and rows[4].text == 'L4', 'add at the change')
  check(rows[5].text == 'l5' and rows[6].text == 'l6', 'only the first 2 suffix lines survive')
  local first = T.hunk_rows('a\nb', 'X\nb')
  check(#first == 3 and first[1].text == 'a' and first[3].text == 'b', 'change on the first line: no leading context')
  local last = T.hunk_rows('a\nb', 'a\nX')
  check(#last == 3 and last[1].text == 'a' and last[3].text == 'X', 'change on the last line: no trailing context')
  check(#T.hunk_rows('same\ntext', 'same\ntext') == 0, 'identical inputs yield an empty hunk')
end)

test('hunk: pure deletion, pure insertion, trailing-newline normalization', function()
  local del = T.hunk_rows('a\nb', '')
  check(kinds(del) == 'del,del', 'empty new_string: deletion rows only')
  local ins = T.hunk_rows('', 'a\nb')
  check(kinds(ins) == 'add,add', 'empty old_string: insertion rows only')
  local norm = T.hunk_rows('a\n', 'b\n')
  check(#norm == 2 and norm[1].text == 'a' and norm[2].text == 'b', 'one trailing newline is trimmed, not an empty row')
  check(#T.hunk_rows('x', 'x\n') == 0, 'a trailing-newline difference alone is not a change')
  local nul = T.hunk_rows('a\0b', 'a\0c')
  check(nul[1].text == 'a\\0b', 'NUL escaped so the row stays one buffer row')
  local single_row = true
  for _, r in ipairs(T.hunk_rows('a\0b\nq\nr\ns', 'a\0c\nq\nr\ns')) do
    if r.text:find('\n', 1, true) then single_row = false end
  end
  check(single_row, 'no row text carries an embedded newline')
end)

test('hunk: replace_all never changes the rows', function()
  local off = T.scan_edit_args('{"old_string":"a\\nb","new_string":"a\\nB","replace_all":false}')
  local on = T.scan_edit_args('{"old_string":"a\\nb","new_string":"a\\nB","replace_all":true}')
  check(off.replace_all == false and on.replace_all == true, 'the flag is parsed either way')
  local r_off = T.hunk_rows(off.old_string, off.new_string)
  local r_on = T.hunk_rows(on.old_string, on.new_string)
  check(kinds(r_off) == 'context,del,add' and kinds(r_on) == 'context,del,add', 'same row kinds')
  check(#r_off == #r_on and r_on[1].text == 'a' and r_on[3].text == 'B', 'same row texts')
end)

-- ------------------------------------------------------------------ windowing

test('window: settled args keep the head, streaming keeps the tail', function()
  local rows = {}
  for i = 1, 14 do rows[i] = { kind = 'context', text = 'r' .. i } end
  local settled = T.diff_window(rows, 10, false)
  check(#settled == 11, '10 rows plus one marker')
  check(settled[1].text == 'r1' and settled[10].text == 'r10', 'head of the hunk kept')
  check(settled[11].kind == 'marker', 'marker follows the rows')
  check(settled[11].text == '... 4 more diff lines (press o for full diff)', 'settled marker text')
  local stream = T.diff_window(rows, 10, true)
  check(#stream == 11, '10 rows plus one marker')
  check(stream[1].kind == 'marker', 'marker precedes the rows')
  check(stream[1].text == '... 4 earlier diff lines (press o for full diff)', 'streaming marker text')
  check(stream[2].text == 'r5' and stream[11].text == 'r14', 'tail of the hunk kept')
  check(stream[1].text:sub(1, 2) ~= '► ', 'the marker is content, not a chrome row')
end)

test('window: cap nil is unlimited, cap >= rows adds no marker', function()
  local rows = {}
  for i = 1, 14 do rows[i] = { kind = 'del', text = 'd' .. i } end
  local full = T.diff_window(rows, nil, false)
  check(#full == 14 and full[14].text == 'd14', 'cap nil: every row, no marker')
  check(#T.diff_window(rows, nil, true) == 14, 'cap nil ignores the streaming bias')
  local exact = { rows[1], rows[2], rows[3], rows[4], rows[5], rows[6], rows[7], rows[8], rows[9], rows[10] }
  local no_marker = T.diff_window(exact, 10, false)
  check(#no_marker == 10 and no_marker[10].kind == 'del', 'exactly cap rows: everything, no marker')
  check(#T.diff_window(exact, 10, true) == 10, 'exactly cap rows while streaming: no marker either')
  local one_over = {}
  for i = 1, 11 do one_over[i] = { kind = 'add', text = 'a' .. i } end
  local w = T.diff_window(one_over, 10, false)
  check(#w == 11 and w[11].text == '... 1 more diff line (press o for full diff)', 'one omitted row: count and noun are singular')
  local s = T.diff_window(one_over, 10, true)
  check(#s == 11 and s[1].text == '... 1 earlier diff line (press o for full diff)', 'streaming count and noun are singular')
  check(#T.diff_window({}, 10, false) == 0, 'no rows: nothing, not a marker')
end)

-- ---------------------------------------------------------------- rendering
-- Buffer-level tests: drive the reducer with real tool-call events and assert
-- the projected rows, the row map, and the full-row highlight groups. The
-- buffer is the only place the whole pipeline (scan, hunk, window, layout,
-- extmarks) can disagree, so these are the integration cases.

local TC_FENCE = string.rep('`', 10)

-- Apply one event and render the resulting diff into the buffer.
local function apply_render(m, b, event)
  local d = T.apply(m, event)
  T.render(m, d, { buf = b, ns = ns, bulk = false })
end

-- A fresh model plus a scratch buffer seeded like a display buffer.
local function new_scene()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  return m, b
end

-- Start a tool call and stream its raw args as one chunk; returns the element.
local function stream_tool(m, b, args, name)
  apply_render(m, b, { AssistantToolCallStart = {
    tool_call_id = 't1', tool_name = name or 'edit', tool_call_index = 0 } })
  local el = m.elements[#m.elements]
  if args and args ~= '' then
    apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = args } })
  end
  return el
end

-- Minimal edit args JSON (only the keys the diff reads, \n escaped).
local function json_edit(fields)
  local parts = {}
  for _, k in ipairs({ 'file_path', 'old_string', 'new_string' }) do
    if fields[k] then
      parts[#parts + 1] = '"' .. k .. '":"' .. fields[k]:gsub('\n', '\\n') .. '"'
    end
  end
  if fields.replace_all ~= nil then
    parts[#parts + 1] = '"replace_all":' .. tostring(fields.replace_all)
  end
  return '{' .. table.concat(parts, ',') .. '}'
end

-- Layout row texts joined, for comparing two element_layout runs.
local function layout_text(layout)
  local out = {}
  for _, row in ipairs(layout) do out[#out + 1] = row.text end
  return table.concat(out, '\n')
end

-- Full-row marks of the display namespace, indexed by buffer row:
-- rows[row] = { { group = ..., priority = ... }, ... }.
local function row_marks(b)
  local rows = {}
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    local d = mm[4]
    if d and d.end_row == mm[2] + 1 and d.end_col == 0 then
      rows[mm[2]] = rows[mm[2]] or {}
      table.insert(rows[mm[2]], { group = d.hl_group, priority = d.priority })
    end
  end
  return rows
end

local function row_group(marks, row)
  local list = marks[row]
  if not list or #list ~= 1 then return nil end
  return list[1].group, list[1].priority
end

test('render: complete edit args show the diff inside the fence pair', function()
  local m, b = new_scene()
  local tc = stream_tool(m, b, ARGS)
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1',
    content = 'Successfully edited /repo/src/a.rs (1 replacement)' } })
  apply_render(m, b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded',
    input_tokens = 12, output_tokens = 34 } })
  local want = {
    '► TOOL: [done] edit',
    '► Diff /repo/src/a.rs',
    TC_FENCE,
    '  fn a() {',
    '-     one();',
    '+     two();',
    '  }',
    TC_FENCE,
    '► Result',
    TC_FENCE,
    'Successfully edited /repo/src/a.rs (1 replacement)',
    TC_FENCE,
    '► [TOOL: 12 in / 34 out tokens]',
  }
  local l = lines_of(b)
  check(#l == #want, 'buffer has exactly the expected rows')
  for i = 1, #want do
    check(l[i] == want[i], ('row %d is %q'):format(i, want[i]))
  end
  local state = T.get_renderer_state(m)
  check(state.rows[tc.id].start_row == 0 and state.rows[tc.id].height == 12,
    'row map: the tool owns its 12 projected rows')
  check(state.rows[m.elements[2].id].start_row == 12, 'end_info starts below the tool region')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('render: streaming shows removals, then additions, then settles', function()
  local m, b = new_scene()
  local tc = stream_tool(m, b, '{"file_path":"/repo/src/a.rs","old_string":"fn a() {\\n    one();\\n}\\n"')
  local l = lines_of(b)
  check(l[2] == '► Diff /repo/src/a.rs', 'header appears as soon as the path is decoded')
  check(l[4] == '- fn a() {' and l[5] == '-     one();' and l[6] == '- }',
    'only - rows while new_string has not started')
  check(#l == 7 and l[3] == TC_FENCE and l[7] == TC_FENCE, 'fence pair stays paired while streaming')
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0,
    content = ',"new_string":"fn a() {\\n    two();\\n}\\n"' } })
  l = lines_of(b)
  check(l[4] == '  fn a() {' and l[5] == '-     one();' and l[6] == '+     two();' and l[7] == '  }',
    'added rows appear once new_string decodes')
  check(tc.args_open == true, 'args still streaming')
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  check(tc.args_open == false, 'args closed')
  l = lines_of(b)
  check(l[2] == '► Diff /repo/src/a.rs' and l[5] == '-     one();' and l[6] == '+     two();',
    'settled hunk is the same hunk')
  check(#l == 12 and l[9] == '► Result', 'Result section opens below the diff')
end)

test('render: the pane window is tail-biased while streaming, head-biased after', function()
  local old_rows, new_rows = {}, {}
  for i = 1, 14 do
    old_rows[i] = 'old' .. i
    new_rows[i] = 'new' .. i
  end
  local args = json_edit({
    file_path = '/big.rs',
    old_string = table.concat(old_rows, '\n'),
    new_string = table.concat(new_rows, '\n'),
  })
  local m, b = new_scene()
  local tc = stream_tool(m, b, args)
  local l = lines_of(b)
  -- 28 rows, cap 10, streaming: the newest rows win and the marker leads.
  check(#l == 15, 'label + header + fence + 10 rows + marker + fence')
  check(l[4] == '... 18 earlier diff lines (press o for full diff)', 'streaming marker leads')
  check(l[5] == '+ new5' and l[14] == '+ new14', 'the last 10 rows are shown')
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  check(tc.args_open == false, 'args closed')
  l = lines_of(b)
  check(l[4] == '- old1' and l[13] == '- old10', 'settled: the head of the hunk is shown')
  check(l[14] == '... 18 more diff lines (press o for full diff)', 'settled marker trails')
  check(l[15] == TC_FENCE, 'closing fence after the marker')
end)

test('render: replace_all only changes the header note', function()
  local m, b = new_scene()
  stream_tool(m, b, json_edit({
    file_path = '/f.rs', old_string = 'a\nb', new_string = 'a\nB', replace_all = true }))
  local l = lines_of(b)
  check(l[2] == '► Diff /f.rs (all occurrences)', 'header carries the all-occurrences note')
  check(l[4] == '  a' and l[5] == '- b' and l[6] == '+ B', 'rows are the same hunk')
  local m2, b2 = new_scene()
  stream_tool(m2, b2, json_edit({
    file_path = '/f.rs', old_string = 'a\nb', new_string = 'a\nB', replace_all = false }))
  check(lines_of(b2)[2] == '► Diff /f.rs', 'replace_all false leaves the header bare')
  check(#lines_of(b2) == #lines_of(b), 'the flag never changes the height')
end)

test('render: the detail window shows every row, no marker', function()
  local old_rows, new_rows = {}, {}
  for i = 1, 6 do
    old_rows[i] = 'line' .. i
    new_rows[i] = 'LINE' .. i
  end
  local args = json_edit({
    file_path = '/big.rs',
    old_string = table.concat(old_rows, '\n'),
    new_string = table.concat(new_rows, '\n'),
  })
  local m, b = new_scene()
  m.full_input = true
  local tc = stream_tool(m, b, args)
  check(tc.full_input == true, 'the element inherited full_input from the model')
  local l = lines_of(b)
  check(#l == 16, 'label + header + fence + 12 rows + fence, nothing capped')
  check(l[4] == '- line1' and l[15] == '+ LINE6', 'first and last row both present')
  local marker = false
  for _, line in ipairs(l) do
    if line:find('diff lines (press o', 1, true) then marker = true end
  end
  check(not marker, 'no marker row in the detail window')
  check(T.get_renderer_state(m).rows[tc.id].height == 16, 'detail row map covers every row')

  local pm, pb = new_scene()
  stream_tool(pm, pb, args)
  apply_render(pm, pb, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  local pl = lines_of(pb)
  check(#pl == 19 and pl[14] == '... 2 more diff lines (press o for full diff)',
    'the pane caps the same hunk at 10 rows')
  check(T.get_renderer_state(pm).rows[pm.elements[1].id].height == 19,
    'pane row map covers the window')
end)

test('render: every diff row carries its own full-row group', function()
  local m, b = new_scene()
  local tc = stream_tool(m, b, ARGS)
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1',
    content = 'Successfully edited /repo/src/a.rs (1 replacement)' } })
  local marks = row_marks(b)
  local g, prio = row_group(marks, 3)
  check(g == 'TCodeToolArgs' and prio == 150, 'context row: TCodeToolArgs at priority 150')
  g, prio = row_group(marks, 4)
  check(g == 'TCodeDiffDelete' and prio == 150, '- row: TCodeDiffDelete at priority 150')
  g, prio = row_group(marks, 5)
  check(g == 'TCodeDiffAdd' and prio == 150, '+ row: TCodeDiffAdd at priority 150')
  g = row_group(marks, 6)
  check(g == 'TCodeToolArgs', 'trailing context row keeps the dim group')
  local rh = T.element_row_highlights(tc, {})
  check(#rh == 4 and rh[1].row == 3 and rh[4].row == 6, 'the layout claims exactly the four diff rows')
  check(rh[2].group == 'TCodeDiffDelete' and rh[3].group == 'TCodeDiffAdd', 'row kinds map to their groups')
  check(marks[2] == nil and marks[7] == nil, 'fence rows carry no full-row group')
  check(marks[1] == nil, 'the header row is chrome (col spans only)')
  check(row_group(marks, 10) == 'TCodeToolArgs', 'the Result content row keeps the per-type fallback')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('render: diff payloads that look structural are still content rows', function()
  local m, b = new_scene()
  stream_tool(m, b, json_edit({
    file_path = '/f.rs',
    old_string = '► not a header\n' .. TC_FENCE,
    new_string = 'changed',
  }))
  local l = lines_of(b)
  check(l[4] == '- ► not a header' and l[5] == '- ' .. TC_FENCE and l[6] == '+ changed',
    'a chrome-looking and a fence-looking payload keep their diff prefixes')
  local marks = row_marks(b)
  check(row_group(marks, 3) == 'TCodeDiffDelete', 'the ► payload row keeps its - group')
  check(row_group(marks, 4) == 'TCodeDiffDelete', 'the fence payload row is not mistaken for a fence')
  check(row_group(marks, 5) == 'TCodeDiffAdd', 'the + payload row keeps its + group')
end)

test('render: the marker row is content with the TCodeTokens group', function()
  local old_rows, new_rows = {}, {}
  for i = 1, 14 do
    old_rows[i] = 'x' .. i
    new_rows[i] = 'y' .. i
  end
  local m, b = new_scene()
  stream_tool(m, b, json_edit({
    file_path = '/f.rs',
    old_string = table.concat(old_rows, '\n'),
    new_string = table.concat(new_rows, '\n'),
  }))
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  local l = lines_of(b)
  local marker_row
  for i, line in ipairs(l) do
    if line:find('more diff lines (press o', 1, true) then marker_row = i - 1 end
  end
  check(marker_row ~= nil, 'the pane shows a truncation marker')
  check(l[marker_row + 1]:sub(1, 2) ~= '► ', 'the marker is not a chrome row')
  local marks = row_marks(b)
  check(row_group(marks, marker_row) == 'TCodeTokens', 'the marker row is TCodeTokens')
end)

test('render: non-edit tools keep the Param tail', function()
  local m, b = new_scene()
  stream_tool(m, b, ARGS, 'bash')
  local l = lines_of(b)
  check(l[2] == '► Param' and l[3] == TC_FENCE and l[4] == ARGS and l[5] == TC_FENCE,
    'a bash call still renders the raw args inside the Param fence')
  check(#l == 5 and l[1] == '► TOOL: [generating] bash  [Ctrl-k to cancel]', 'no extra rows appear')

  local mm, mb = new_scene()
  stream_tool(mm, mb, 'a\nb\nc\nd\ne\nf', 'read')
  local ml = lines_of(mb)
  check(ml[2] == '► Param' and ml[4] == 'b' and ml[8] == 'f', 'the 5-line tail window is unchanged')
  check(#ml == 9, 'label + header + fence + 5 rows + fence')
end)

test('render: the edit match is the last name segment only', function()
  local nm, nb = new_scene()
  stream_tool(nm, nb, ARGS, 'mcp.server.edit')
  check(lines_of(nb)[2] == '► Diff /repo/src/a.rs', 'a namespaced name still matches edit')
  local om, ob = new_scene()
  stream_tool(om, ob, ARGS, 'my_edit')
  check(lines_of(ob)[2] == '► Param', 'a name that merely ends in edit does not match')
  local em, eb = new_scene()
  stream_tool(em, eb, ARGS, '')
  check(lines_of(eb)[2] == '► Param', 'an empty tool name is never edit')
end)

test('render: an edit without decoded values keeps the raw tail', function()
  local m, b = new_scene()
  stream_tool(m, b, '{"file_path": "/rep')
  local l = lines_of(b)
  check(l[2] == '► Param' and l[4] == '{"file_path": "/rep', 'undecoded args render as the raw tail')
  check(#l == 5, 'the Param section is still the 5-row shape')
  local m2, b2 = new_scene()
  stream_tool(m2, b2, '{"old_string":"","new_string":""}')
  local l2 = lines_of(b2)
  check(l2[2] == '► Param', 'two empty diff strings have nothing to show')
end)

test('render: a malformed value never freezes the block', function()
  clear_errors()
  local m, b = new_scene()
  local tc = stream_tool(m, b, MALFORMED_ARG)
  local l = lines_of(b)
  check(l[4] == '- a\\u-1f0b' and l[5] == '+ c', 'an invalid \\u escape renders verbatim')
  check(T.get_renderer_state(m).rows[tc.id].height == 6, 'the block grew past its label row')
  check(#recorded_errors == 0, 'no render error was reported for the bad escape')
  local m2, b2 = new_scene()
  local tc2 = stream_tool(m2, b2, BOOLEAN_ARG)
  check(lines_of(b2)[4] == '+ b', 'a boolean old_string is ignored, the decoded new_string renders')
  check(T.get_renderer_state(m2).rows[tc2.id].height == 5, 'the block grew past its label row')
  check(#recorded_errors == 0, 'no render error was reported for the bad value')
end)

test('render: identical old and new strings render empty fences', function()
  local m, b = new_scene()
  stream_tool(m, b, json_edit({ file_path = '/f.rs', old_string = 'same', new_string = 'same' }))
  local l = lines_of(b)
  check(l[2] == '► Diff /f.rs', 'header renders')
  check(#l == 4 and l[3] == TC_FENCE and l[4] == TC_FENCE,
    'an empty hunk is an empty fenced body, not a crash')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('render: an edit with empty args renders no section', function()
  local m, b = new_scene()
  local tc = stream_tool(m, b, '')
  local l = lines_of(b)
  check(#l == 1 and l[1] == '► TOOL: [generating] edit  [Ctrl-k to cancel]', 'label only')
  check(T.get_renderer_state(m).rows[tc.id].height == 1, 'row map: one row')
  check(#row_marks(b) == 0, 'no row marks at all')
end)

test('render: diff rows keep later elements and the row map honest', function()
  local m, b = new_scene()
  local tc = stream_tool(m, b, ARGS)
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'ok' } })
  apply_render(m, b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded',
    input_tokens = 12, output_tokens = 34 } })
  local info = m.elements[#m.elements]
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't2', tool_name = 'bash',
    tool_call_index = 1 } })
  local tc2 = m.elements[#m.elements]
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 1, content = 'x' } })
  local state = T.get_renderer_state(m)
  check(state.rows[info.id].start_row == 12, 'end_info sits right below the diff section')
  check(state.rows[tc2.id].start_row == 13, 'the next tool call starts after end_info')
  for row = 3, 6 do
    local el, off = T.element_at_row_full(m, b, row)
    check(el == tc and off == row, ('diff row %d resolves to the tool call'):format(row))
    check(T.action_at(el) == 'detail', ('diff row %d opens the detail view'):format(row))
  end
  local tc2_row = state.rows[tc2.id]
  check(#lines_of(b) == tc2_row.start_row + tc2_row.height,
    'the buffer height matches the last element row entry')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('render: the diff cache refreshes when the args grow', function()
  local m, b = new_scene()
  local tc = stream_tool(m, b, '{"file_path":"/f.rs","old_string":"a\\nb","new_string":"a\\nB"')
  local first = layout_text(T.element_layout(tc, {}))
  check(layout_text(T.element_layout(tc, {})) == first, 'unchanged args: same layout')
  local e1 = T.edit_diff_for(tc)
  check(T.edit_diff_for(tc) == e1, 'the cache hands back the same entry')
  check(T.edit_diff_usable(e1.meta) == true, 'a decoded old_string is usable')
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0,
    content = ',"replace_all":true}' } })
  local grown = layout_text(T.element_layout(tc, {}))
  check(grown ~= first, 'a longer args text invalidates the cached diff')
  check(grown:find('(all occurrences)', 1, true) ~= nil, 'the refreshed layout sees replace_all')
  check(T.edit_diff_for(tc) ~= e1, 'a new entry replaced the stale one')
end)
