-- End-to-end regression tests for the 500ms settle flush (flush_deferred in
-- create_jsonl_reader). These drive the REAL reader: a JSONL file on disk,
-- reader.check() reading it, the real vim.schedule batch render, and the real
-- uv timer armed by arm_flush_timer. vim.wait pumps the loop so the scheduled
-- callback and the timer actually fire.
--
-- Regression: this is the path that used to raise
--   vim.schedule callback: ... Buffer is not 'modifiable'
-- when a session file went quiet with an unterminated thinking block or an
-- open args fence (interrupted / attached sessions).

local TC_FENCE = string.rep('`', 10)

-- An open thinking block is the model tail with state 'open'.
local function is_open()
  local tail = T.model.tail
  return tail and tail.type == 'thinking_block' and tail.state == 'open'
end

local function write_jsonl(path, ...)
  local file = assert(io.open(path, 'w'))
  for _, line in ipairs({ ... }) do
    file:write(line, '\n')
  end
  file:close()
end

test('flush: JSONL ending mid-thinking auto-collapses without errors', function()
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

  -- Initial bulk render runs in the scheduled batch callback.
  local loaded = vim.wait(500, is_open)
  -- The 500ms settle timer then collapses the unterminated thinking block.
  local flushed = vim.wait(1500, function() return not is_open() end)
  check(loaded, 'initial load rendered the thinking block')
  check(flushed, 'settle flush collapsed the unterminated thinking block')
  check(#recorded_errors == 0, 'no error reported during load/flush')
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT', 'assistant label rendered')
  check(l[2] == '' and l[3] == '' and l[4] == '', 'thinking collapsed to indicator rows')
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})
  check(#marks >= 1, 'thinking indicator extmark present')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable after flush')
end)

test('flush: JSONL ending with an open args fence gets it closed', function()
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

  -- Pump the loop: batch render, then the settle flush closes the fence.
  vim.wait(1000)
  check(#recorded_errors == 0, 'no error reported during load/flush')
  local l = lines_of(b)
  local fence_count = 0
  for _, line in ipairs(l) do
    if line == TC_FENCE then
      fence_count = fence_count + 1
    end
  end
  check(fence_count >= 2, 'args fence opened and closed by the flush')
  -- The flush closes the fence and collapses long args to a single preview
  -- row (escaped, backslash-n) instead of the full 4 content rows.
  local preview_row = nil
  for _, line in ipairs(l) do
    if line:find('a\\nb\\nc\\nd', 1, true) then preview_row = line end
  end
  check(preview_row ~= nil, 'args collapsed to an escaped preview row by the flush')
  check(l[5] ~= 'a' and l[6] ~= 'b' and l[7] ~= 'c' and l[8] ~= 'd', 'full args rows not materialized')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable after flush')
end)

test('flush: pause between reasoning bursts merges into a single thinking entry', function()
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
  -- First burst renders, then the 500ms settle flush collapses it (this is
  -- the split point: one continuous reasoning stream paused mid-turn).
  local loaded = vim.wait(500, is_open)
  local collapsed = vim.wait(1500, function() return not is_open() end)
  check(loaded, 'first burst rendered')
  check(collapsed, 'first burst collapsed by the settle flush')

  -- The stream resumes: the second burst must merge into the existing entry
  -- and stream visibly, not open a second collapsed block.
  local file = assert(io.open(jsonl, 'a'))
  file:write('{"AssistantThinkingChunk":{"content":"\\nburst two"}}\n')
  file:close()
  check_file()
  local streaming = vim.wait(500, is_open)
  check(streaming, 'second burst merged and streaming')
  local l = lines_of(b)
  check(l[2] == '' and l[3] == 'burst two', 'second burst streams visibly at the merged anchor')

  -- Final settle flush collapses the merged run into ONE entry.
  local done = vim.wait(1500, function() return not is_open() end)
  check(done, 'merged run collapsed by the settle flush')
  local entries = {}
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' and el.state ~= 'open' then
      entries[#entries + 1] = el
    end
  end
  check(#entries == 1, 'both bursts merged into a single thinking entry')
  check(entries[1] and entries[1].content == 'burst one\nburst two', 'merged entry holds both bursts in order')
  check(#recorded_errors == 0, 'no errors during the merge')
end)

test('flush: live append after initial load keeps the modifiable invariant', function()
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
  -- Wait out the settle timer this check_file armed: leaving it pending would
  -- fire flush_deferred into the next test. It would be a no-op there (the
  -- next test starts with a fresh model), but drain it anyway so timers never
  -- pile up across tests.
  vim.wait(600)
end)
