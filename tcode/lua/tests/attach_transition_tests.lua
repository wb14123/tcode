-- Attach/interruption scenarios: thinking blocks that stream during the
-- initial bulk load, end, or transition to live streaming. The display opens
-- on a session file that may end mid-thinking (interrupted session) or keep
-- growing (live session); the thinking block must collapse cleanly without
-- swallowing blocks below it.

test('attach scenario: bulk thinking then SubAgentStart', function()
  local b = new_buf()
  T.reset_first_event()
  seed(b, { '' })
  local ok1 = pcall(windowed_render, b, { AssistantMessageStart = {} }, true)
  local ok2 = pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'secret thinking' } }, true)
  local ok3 = pcall(windowed_render, b, {
    SubAgentStart = { description = 'sub', tool_call_id = 't1', conversation_id = 'c1' },
  }, true)
  check(ok1 and ok2 and ok3, 'rendering bulk thinking -> SubAgentStart raises no errors')
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT', 'assistant label rendered')
  check(l[2] == '' and l[3] == '' and l[4] == '', 'thinking collapsed to indicator rows')
  check(l[5] == '► SUBAGENT' and l[6] == '', 'subagent label intact below the thinking block')
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})
  check(#marks >= 1 and marks[1][2] == 1, 'thinking indicator extmark at row 1')
  reset_thinking()
end)

test('thinking is collapsed before a new user message (crash/resume flow)', function()
  local b = new_buf()
  T.reset_first_event()
  seed(b, { '' })
  -- The session file ended mid-thinking: bulk load leaves the thinking block
  -- open (no collapse point in the file).
  pcall(windowed_render, b, { AssistantMessageStart = {} }, true)
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'old thinking' } }, true)
  check(T.thinking_state.is_thinking == true, 'thinking open after bulk load')
  -- The session resumes and the user sends a message before the settle flush
  -- fires; the new turn must not merge into the unterminated block.
  pcall(windowed_render, b, { UserMessage = { content = 'hello again' } }, false)
  check(T.thinking_state.is_thinking == false, 'thinking collapsed at UserMessage')
  pcall(windowed_render, b, { AssistantMessageStart = {} }, false)
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'new thinking' } }, false)
  local l = lines_of(b)
  check(l[2] == '' and l[3] == '' and l[4] == '', 'old thinking collapsed to indicator rows')
  check(table.concat(l, '|'):find('hello again', 1, true) ~= nil, 'user message preserved below the indicator')
  check(table.concat(l, '|'):find('new thinking', 1, true) ~= nil, 'new thinking appended after the user message')
  reset_thinking()
end)

test('bulk to live transition: written flips and collapse covers live rows', function()
  local b = new_buf()
  T.reset_first_event()
  seed(b, { '' })
  pcall(windowed_render, b, { AssistantMessageStart = {} }, true)
  -- Bulk chunks are not written to the buffer (content deferred).
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'from file' } }, true)
  check(T.thinking_state.written == false, 'written=false after bulk chunk')
  -- The session is alive: live chunks now write to the buffer.
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = ' live part\nsecond line' } }, false)
  check(T.thinking_state.written == true, 'written flips to true on the first live chunk')
  -- The next collapse point must collapse the thinking rows (bulk + live)
  -- without touching the message chunk appended after it.
  pcall(windowed_render, b, { AssistantMessageChunk = { content = ' reply' } }, false)
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT', 'assistant label intact')
  check(l[2] == '' and l[3] == '', 'thinking rows collapsed (content-derived range)')
  check(l[4] == ' reply', 'message chunk appended after the collapse')
  reset_thinking()
end)
