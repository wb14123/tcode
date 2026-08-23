-- Attach/interruption scenarios: thinking blocks that stream during the
-- initial bulk load, end, or transition to live streaming. The display opens
-- on a session file that may end mid-thinking (interrupted session) or keep
-- growing (live session); the thinking block must collapse cleanly without
-- swallowing blocks below it. The initial bulk load is a one-time full
-- projection (everything materialized), so these assert on real lines + the
-- integer row map.

local TC_FENCE = string.rep('`', 10)

local function row_of(m, el)
  return T.get_renderer_state(m).rows[el.id]
end

local function is_open_thinking()
  local tail = T.model.tail
  return tail and tail.type == 'thinking_block' and tail.state == 'open'
end

test('attach scenario: bulk thinking then SubAgentStart', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local ok1 = pcall(windowed_render, b, { AssistantMessageStart = {} }, true)
  local ok2 = pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'secret thinking' } }, true)
  local ok3 = pcall(windowed_render, b, {
    SubAgentStart = { description = 'sub', tool_call_id = 't1', conversation_id = 'c1' },
  }, true)
  check(ok1 and ok2 and ok3, 'rendering bulk thinking -> SubAgentStart raises no errors')
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT', 'assistant label rendered')
  check(l[2] == '► [Thinking... press o to expand]', 'thinking collapsed to one real row')
  -- No SubAgentInputStart was streamed: the reducer adds a fenced fallback
  -- subagent region (label + Output header + fence + empty output) below the
  -- thinking block; the subagent is still streaming, so no close fence.
  check(l[3] == '► SUB-AGENT: [running]  sub' and l[4] == '► Output' and l[5] == TC_FENCE and l[6] == '',
    'subagent label + fenced fallback region below the thinking block')
  local block, sa
  for _, el in ipairs(m.elements) do
    if el.type == 'thinking_block' then block = el end
    if el.type == 'subagent' then sa = el end
  end
  check(row_of(m, block).start_row == 1 and row_of(m, block).height == 1,
    'row map: collapsed block at [1, 2)')
  check(row_of(m, sa).start_row == 2 and row_of(m, sa).height == 4,
    'row map: subagent region at [2, 6)')
  local blocks, subs = 0, 0
  for _, el in ipairs(m.elements) do
    if el.type == 'thinking_block' and el.state == 'collapsed' then blocks = blocks + 1 end
    if el.type == 'subagent' then subs = subs + 1 end
  end
  check(blocks == 1 and subs == 1, 'model holds the collapsed block + the subagent element')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('thinking is collapsed before a new user message (crash/resume flow)', function()
  local m = T.reset_model()
  local b = new_buf()
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
  check(l[2] == '► [Thinking... press o to expand]', 'old thinking collapsed to one real row')
  check(l[4] == 'hello again', 'user message preserved below the indicator')
  check(l[6] == 'new thinking', 'new thinking appended after the user message')
  local first_collapsed, has_user, second_open = false, false, false
  local seen_block = 0
  for _, el in ipairs(m.elements) do
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

test('bulk to live transition: the full projection materializes, collapse covers live rows', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  pcall(windowed_render, b, { AssistantMessageStart = {} }, true)
  -- The initial bulk load writes EVERYTHING in one full projection: the bulk
  -- thinking content is materialized immediately, not deferred.
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = 'from file' } }, true)
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'from file',
    'bulk load materialized the thinking content in one shot')
  -- The session is alive: live chunks stream onto the thinking block's OWN rows.
  pcall(windowed_render, b, { AssistantThinkingChunk = { content = ' live part\nsecond line' } }, false)
  l = lines_of(b)
  check(l[2] == 'from file live part' and l[3] == 'second line', 'live chunks stream onto the block rows')
  -- The next collapse point must collapse the thinking rows without touching
  -- the message chunk appended after it.
  pcall(windowed_render, b, { AssistantMessageChunk = { content = ' reply' } }, false)
  l = lines_of(b)
  check(l[1] == '► ASSISTANT', 'assistant label intact')
  -- The thinking block is ATTACHED inside the am: it stays above the reply,
  -- and the reply lands below it (arrival order: label, thinking, response).
  check(l[2] == '► [Thinking... press o to expand]', 'thinking collapsed to one real row above the reply')
  check(l[3] == ' reply', 'message chunk appended below the attached collapsed hint')
  local block = nil
  local am = nil
  for _, el in ipairs(m.elements) do
    if el.type == 'thinking_block' then block = el end
    if el.type == 'assistant_message' then am = el end
  end
  check(block ~= nil and block.state == 'collapsed', 'model block collapsed')
  check(am ~= nil and T.content_of(am, 'content') == ' reply', 'model assistant message holds the reply')
  check(row_of(m, am).start_row == 0 and row_of(m, am).height == 3,
    'row map: assistant region [0, 3) = label + attached block + reply')
  check(row_of(m, block).start_row == 1 and row_of(m, block).height == 1,
    'row map: collapsed block attached at [1, 2) inside the am')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)
