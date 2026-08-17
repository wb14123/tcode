-- Attach/interruption scenarios: thinking blocks that stream during the
-- initial bulk load, end, or transition to live streaming. The display opens
-- on a session file that may end mid-thinking (interrupted session) or keep
-- growing (live session); the thinking block must collapse cleanly without
-- swallowing blocks below it.

local TC_FENCE = string.rep('`', 10)

local function is_open_thinking()
  local tail = T.model.tail
  return tail and tail.type == 'thinking_block' and tail.state == 'open'
end

test('attach scenario: bulk thinking then SubAgentStart', function()
  local b = new_buf()
  T.reset_model()
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
  -- No SubAgentInputStart was streamed: the reducer adds a fenced fallback
  -- subagent region (label + input fence + output fence), the plan-mandated
  -- layout for resumed sessions.
  check(l[5] == '► SUBAGENT' and l[6] == TC_FENCE, 'subagent label + fenced fallback region below the thinking block')
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})
  check(#marks >= 1 and marks[1][2] == 1, 'thinking indicator extmark at row 1')
  local blocks, subs = 0, 0
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' and el.state == 'collapsed' then blocks = blocks + 1 end
    if el.type == 'subagent' then subs = subs + 1 end
  end
  check(blocks == 1 and subs == 1, 'model holds the collapsed block + the subagent element')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('thinking is collapsed before a new user message (crash/resume flow)', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { '' })
  -- The session file ended mid-thinking: bulk load leaves the thinking block
  -- open (no collapse point in the file).
  pcall(windowed_render, b, { AssistantMessageStart = {} }, true)
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'old thinking' } }, true)
  check(is_open_thinking(), 'thinking open after bulk load')
  -- The session resumes and the user sends a message before the settle flush
  -- fires; the new turn must not merge into the unterminated block.
  pcall(windowed_render, b, { UserMessage = { content = 'hello again' } }, false)
  check(not is_open_thinking(), 'thinking collapsed at UserMessage')
  pcall(windowed_render, b, { AssistantMessageStart = {} }, false)
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'new thinking' } }, false)
  local l = lines_of(b)
  check(l[2] == '' and l[3] == '' and l[4] == '', 'old thinking collapsed to indicator rows')
  check(table.concat(l, '|'):find('hello again', 1, true) ~= nil, 'user message preserved below the indicator')
  check(table.concat(l, '|'):find('new thinking', 1, true) ~= nil, 'new thinking appended after the user message')
  local first_collapsed, has_user, second_open = false, false, false
  local seen_block = 0
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' then
      seen_block = seen_block + 1
      if seen_block == 1 and el.state == 'collapsed' then first_collapsed = true end
      if seen_block == 2 and el.state == 'open' then second_open = true end
    end
    if el.type == 'user_message' then has_user = true end
  end
  check(first_collapsed and has_user and second_open, 'model: old block collapsed, user message, new block open')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('bulk to live transition: deferred content materializes, collapse covers live rows', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { '' })
  pcall(windowed_render, b, { AssistantMessageStart = {} }, true)
  -- Bulk chunks are not written to the buffer (content deferred).
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'from file' } }, true)
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '', 'nothing written for the bulk thinking content')
  -- The session is alive: live chunks now write to the buffer.
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = ' live part\nsecond line' } }, false)
  l = lines_of(b)
  check(l[2] == ' live part' and l[3] == 'second line', 'live chunks stream onto the anchor row')
  -- The next collapse point must collapse the thinking rows without touching
  -- the message chunk appended after it.
  pcall(windowed_render, b, { AssistantMessageChunk = { content = ' reply' } }, false)
  l = lines_of(b)
  check(l[1] == '► ASSISTANT', 'assistant label intact')
  check(l[2] == '' and l[3] == '' and l[4] == ' reply', 'thinking collapsed, message chunk appended after the collapse')
  local block = nil
  local am = nil
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' then block = el end
    if el.type == 'assistant_message' then am = el end
  end
  check(block ~= nil and block.state == 'collapsed', 'model block collapsed')
  check(am ~= nil and T.content_of(am, 'content') == ' reply', 'model assistant message holds the reply')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)
