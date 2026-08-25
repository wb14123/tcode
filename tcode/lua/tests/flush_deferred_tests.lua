-- End-to-end reader tests for the JSONL display reader (create_jsonl_reader).
-- These drive the REAL reader: a JSONL file on disk, reader.check() reading
-- it, and the real vim.schedule batch render. vim.wait pumps the loop so the
-- scheduled batch callback actually fires.
--
-- The 500ms settle flush is GONE: a session file ending mid-thinking stays
-- expanded (chrome row + content, `o`-toggleable), paused bursts stay open and
-- the next burst streams into the same block, and every fence section renders
-- a paired (closed) fence — nothing is ever left dangling.

local TC_FENCE = string.rep('`', 10)

local function write_jsonl(path, ...)
  local file = assert(io.open(path, 'w'))
  for _, line in ipairs({ ... }) do
    file:write(line, '\n')
  end
  file:close()
end

test('reader: JSONL ending mid-thinking stays expanded without errors', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { '' })
  clear_errors()
  local jsonl = tmp_dir .. '/mid-thinking.jsonl'
  write_jsonl(jsonl,
    '{"AssistantMessageStart":{}}',
    '{"AssistantThinkingChunk":{"content":"thinking a\\nb"}}')

  local check_file = T.create_jsonl_reader(jsonl, b, ns, nil)
  check_file()

  -- Initial bulk render runs in the scheduled batch callback; no flush timer
  -- exists to wait for (nothing but the batch callback remains).
  local loaded = vim.wait(500, function()
    return vim.api.nvim_buf_line_count(b) > 1
  end)
  check(loaded, 'initial load rendered the thinking block')
  check(#recorded_errors == 0, 'no error reported during load')
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT', 'assistant label rendered')
  check(l[2] == '► [Thinking... press o to collapse]' and l[3] == 'thinking a' and l[4] == 'b',
    'thinking stays expanded (chrome row + full content)')
  local block = nil
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' then block = el end
  end
  check(block ~= nil and block.state == 'expanded', 'block state expanded')
  local r = block and T.get_renderer_state(T.model).rows[block.id]
  check(r and r.start_row == 1 and r.height == 3, 'row map: expanded block at [1, 4)')
  local el, off = T.element_at_row_full(T.model, b, 1)
  check(el == block and off == 0 and T.action_at(block) == 'thinking',
    'the chrome row resolves to (block, 0) and toggles thinking')
  el, off = T.element_at_row_full(T.model, b, 2)
  check(el == block and off == 1 and T.action_at(block) == 'thinking',
    'a content row resolves to (block, 1) and toggles thinking too')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable after load')
end)

test('reader: JSONL ending with an open args fence renders a paired fence', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { '' })
  clear_errors()
  local jsonl = tmp_dir .. '/open-args.jsonl'
  write_jsonl(jsonl,
    '{"AssistantMessageStart":{}}',
    '{"AssistantToolCallStart":{"tool_name":"bash","tool_call_id":"tc1","tool_call_index":1}}',
    '{"AssistantToolCallArgChunk":{"tool_call_index":1,"content":"a\\nb\\nc\\nd"}}')

  local check_file = T.create_jsonl_reader(jsonl, b, ns, nil)
  check_file()

  -- Pump the loop: the batch render completes; no flush exists to wait for.
  vim.wait(500)
  check(#recorded_errors == 0, 'no error reported during load')
  local l = lines_of(b)
  local fence_count = 0
  for _, line in ipairs(l) do
    if line == TC_FENCE then
      fence_count = fence_count + 1
    end
  end
  check(fence_count == 2, 'args render with an open + close fence pair')
  -- The 4 args lines (within the 5-line tail cap) render in full inside the
  -- fence pair, no preview row.
  check(l[3] == '► Param' and l[4] == TC_FENCE, 'Param header + open fence rendered')
  check(l[5] == 'a' and l[6] == 'b' and l[7] == 'c' and l[8] == 'd', 'args materialized in full inside the fence')
  check(l[9] == TC_FENCE, 'close fence after the args rows')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable after load')
end)

test('reader: paused bursts stay expanded; the next burst streams into the same block', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { '' })
  clear_errors()
  local jsonl = tmp_dir .. '/burst-pause.jsonl'
  write_jsonl(jsonl,
    '{"AssistantMessageStart":{}}',
    '{"AssistantThinkingChunk":{"content":"burst one"}}')

  local check_file = T.create_jsonl_reader(jsonl, b, ns, nil)
  check_file()
  -- First burst renders and STAYS expanded: no flush fires on the pause, so
  -- the block never collapses and never re-expands (the flash is gone).
  local loaded = vim.wait(500, function()
    return vim.api.nvim_buf_line_count(b) > 1
  end)
  check(loaded, 'first burst rendered')
  local block = nil
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' then block = el end
  end
  check(block and block.state == 'expanded', 'first burst stays expanded through the pause')

  -- The stream resumes: the second burst streams into the SAME block.
  local file = assert(io.open(jsonl, 'a'))
  file:write('{"AssistantThinkingChunk":{"content":"\\nburst two"}}\n')
  file:close()
  check_file()
  local merged = vim.wait(500, function()
    return block and T.content_of(block, 'content') == 'burst one\nburst two'
  end)
  check(merged, 'second burst streamed into the same block')
  check(#T.model.elements == 2, 'still one thinking element (no second block)')
  local l = lines_of(b)
  check(l[2] == '► [Thinking... press o to collapse]' and l[3] == 'burst one' and l[4] == 'burst two',
    'same block streams both bursts (chrome row + full content)')
  check(#recorded_errors == 0, 'no errors during the live append')
end)

test('reader: live append after initial load keeps the modifiable invariant', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { '' })
  clear_errors()
  local jsonl = tmp_dir .. '/live-append.jsonl'
  write_jsonl(jsonl, '{"UserMessage":{"content":"hello"}}')

  local check_file = T.create_jsonl_reader(jsonl, b, ns, nil)
  check_file()
  vim.wait(300)
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable after initial load')

  -- Session resumes: new events land in the file and a fresh check() reads them.
  local file = assert(io.open(jsonl, 'a'))
  file:write('{"AssistantMessageStart":{}}\n')
  file:write('{"AssistantMessageChunk":{"content":"hi"}}\n')
  file:close()
  check_file()
  vim.wait(300)
  check(#recorded_errors == 0, 'no error reported during live append')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable after live append')
  local l = lines_of(b)
  check(table.concat(l, '\n'):find('hi', 1, true) ~= nil, 'live content rendered')
end)

test('reader: an interrupted tool-call detail file renders a paired (closed) fence', function()
  -- The tool-call detail view shares create_jsonl_reader; its model sets
  -- full_input so args never truncate. A detail file interrupted mid-args must
  -- render a paired (closed) fence — nothing dangling.
  local b = new_buf()
  T.reset_model()
  T.model.full_input = true
  seed(b, { '' })
  clear_errors()
  local jsonl = tmp_dir .. '/detail-interrupted.jsonl'
  write_jsonl(jsonl,
    '{"AssistantToolCallStart":{"tool_name":"bash","tool_call_id":"tc1","tool_call_index":1}}',
    '{"AssistantToolCallArgChunk":{"tool_call_index":1,"content":"{\\"cmd\\":\\"ls\\",\\"dir\\":\\"/tmp\\"}"}}')

  local check_file = T.create_jsonl_reader(jsonl, b, ns, nil)
  check_file()
  vim.wait(500)
  check(#recorded_errors == 0, 'no error reported during load')
  local l = lines_of(b)
  local fence_count = 0
  for _, line in ipairs(l) do
    if line == TC_FENCE then fence_count = fence_count + 1 end
  end
  check(fence_count == 2, 'args render with a paired (open + close) fence')
  check(l[1]:find('TOOL:', 1, true) ~= nil, 'tool label rendered')
  check(l[2] == '► Param' and l[3] == TC_FENCE and l[4] == '{"cmd":"ls","dir":"/tmp"}' and l[5] == TC_FENCE,
    'full args inside the paired fence (full_input, no tail cap)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable after load')
end)
