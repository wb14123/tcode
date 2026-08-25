-- Thinking-block streaming behavior in the two-state model (collapsed /
-- expanded; the `open` state and the merge-reopen path are gone). One
-- continuous reasoning stream (DeepSeek bursts via OpenRouter, Claude
-- back-to-back thinking blocks) stays ONE expanded block for as long as
-- nothing collapses it: a pause alone never folds the block (there is no
-- settle flush anymore), so the next run streams into the same block. When
-- any real content (text, tool labels, subagent sections) sits between the
-- runs, the tail moves and the runs become separate `expanded` blocks.
--
-- A collapsed block receiving new thinking NEVER auto-reopens: content
-- accumulates invisibly in the model and rebuilds in full when toggled
-- expanded.
--
-- Buffer assertions use the integer row map and real lines, never extmarks.
-- Entries are inspected via the MODEL: every thinking block is one entry (the
-- old `state ~= 'open'` filter would count every block in a two-state model,
-- so entries are counted directly — the assertions are still meaningful
-- because the removed merge would have folded the runs into one element).

local function row_of(m, el)
  return T.get_renderer_state(m).rows[el.id]
end

local function entry_count(b)
  local n = 0
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' then n = n + 1 end
  end
  return n
end

local function entry_contents(b)
  local contents = {}
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' then
      contents[#contents + 1] = T.content_of(el, 'content')
    end
  end
  table.sort(contents)
  return contents
end

local function is_expanded()
  local tail = T.model.tail
  return tail and tail.type == 'thinking_block' and tail.state == 'expanded'
end

local function last_thinking(m)
  local el = nil
  for _, e in ipairs(m.elements) do
    if e.type == 'thinking_block' then el = e end
  end
  return el
end

test('streaming: consecutive runs stay one continuously-streaming block', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { 'label', '' })
  -- Run 1 streams live; nothing collapses it (no flush, no collapse point).
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1\nA2' } }, false)
  check(is_expanded(), 'run 1 streaming (expanded)')
  local block = last_thinking(m)
  -- Run 2 arrives after the pause: the same expanded block is still the tail,
  -- so it streams into the SAME block (no collapse, no re-expand, no new
  -- element).
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = '\nB2' } }, false)
  check(is_expanded(), 'run 2 streams into the same block')
  check(#m.elements == 1, 'one thinking element for both runs')
  -- The block projects chrome + the full accumulated content.
  local l = lines_of(b)
  check(l[1] == 'label', 'label row intact')
  check(l[3] == '► [Thinking... press o to collapse]' and l[4] == 'A1'
    and l[5] == 'A2B1' and l[6] == 'B2',
    'chrome row + full content, previous run included')
  local r = row_of(m, block)
  check(r.start_row == 2 and r.height == 4, 'row map: block at [2, 6)')
  -- Final collapse folds it to ONE entry with the combined text.
  T.collapse_thinking(m, block, b, ns)
  check(entry_count(b) == 1, 'single thinking entry after the collapse')
  local contents = entry_contents(b)
  check(contents[1] == 'A1\nA2B1\nB2', 'entry holds both runs in order')
  l = lines_of(b)
  check(l[1] == 'label' and l[2] == '' and l[3] == '► [Thinking... press o to expand]',
    'collapsed to one real row below the seeded blank')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('streaming: visible text between runs keeps separate expanded blocks', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  local block1 = last_thinking(m)
  -- A real text chunk sits between the thinking runs: it collapses the tail
  -- (collapse point) and moves the tail to the assistant message.
  windowed_render(b, { AssistantMessageChunk = { content = 'visible text\n' } }, false)
  check(block1.state == 'collapsed', 'block1 collapsed by the text chunk')
  -- The next thinking run opens a fresh expanded block.
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(is_expanded(), 'second run streaming (expanded)')
  local block2 = last_thinking(m)
  check(entry_count(b) == 2, 'two separate entries kept')
  local contents = entry_contents(b)
  check(contents[1] == 'A1' and contents[2] == 'B1', 'entries hold their own content')
  local l = lines_of(b)
  -- block1 is ATTACHED inside the am, so it renders above the visible text
  -- (arrival order); block2 sits after the text as its own attached block.
  check(l[2] == '► [Thinking... press o to expand]' and l[3] == 'visible text'
    and l[5] == '► [Thinking... press o to collapse]' and l[6] == 'B1',
    'attached block1 hint, visible text, block2 chrome + content in arrival order')
  check(row_of(m, block1).start_row == 1 and row_of(m, block1).height == 1
    and row_of(m, block2).start_row == 4 and row_of(m, block2).height == 2,
    'row map: block1 attached at [1, 2) inside the am, block2 expanded at [4, 6)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('streaming: a tool call between runs keeps separate expanded blocks', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  local block1 = last_thinking(m)
  -- A tool call starts between the thinking runs: it collapses block1 and
  -- moves the tail to the tool call.
  windowed_render(b, { AssistantToolCallStart = { tool_name = 'bash', tool_call_id = 'tc1', tool_call_index = 1 } }, false)
  check(block1.state == 'collapsed', 'block1 collapsed at the tool call')
  -- The next thinking run opens a fresh expanded block (tail is the tool call,
  -- so it is NOT attached to the am).
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(is_expanded(), 'second run streaming (expanded)')
  local block2 = last_thinking(m)
  check(entry_count(b) == 2, 'two separate entries kept (one per thinking phase)')
  local l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]' and l[4] == '► [Thinking... press o to collapse]' and l[5] == 'B1',
    'block1 hint, tool region, block2 chrome + content')
  check(l[3]:find('TOOL', 1, true) ~= nil, 'tool label preserved between the indicators')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('streaming: an expanded previous entry never merges a new run', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  local block1 = last_thinking(m)
  -- A text chunk folds block1 (collapse point) and moves the tail to the am.
  windowed_render(b, { AssistantMessageChunk = { content = ' x' } }, false)
  check(block1.state == 'collapsed', 'block1 collapsed by the text chunk')
  -- The user expands it to read it: a later run must NOT stream into it.
  T.toggle_thinking(m, block1, b, ns)
  check(block1.state == 'expanded', 'block1 user-expanded')
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(is_expanded(), 'new run streaming (expanded)')
  local block2 = last_thinking(m)
  check(block2 ~= block1, 'new run opens a separate block')
  check(entry_count(b) == 2, 'two separate entries kept')
  local blocks = {}
  for _, el in ipairs(m.elements) do
    if el.type == 'thinking_block' then blocks[#blocks + 1] = el end
  end
  check(#blocks == 2 and blocks[1].state == 'expanded' and blocks[2].state == 'expanded',
    'block1 expanded and untouched by the new run')
  check(T.content_of(blocks[1], 'content') == 'A1' and T.content_of(blocks[2], 'content') == 'B1',
    'both entries hold their own content')
  local contents = entry_contents(b)
  check(contents[1] == 'A1' and contents[2] == 'B1', 'both entries hold their own content (sorted)')
  local l = lines_of(b)
  check(l[2] == '► [Thinking... press o to collapse]' and l[3] == 'A1',
    'block1 expanded and still visible on screen')
  check(l[5] == '► [Thinking... press o to collapse]' and l[6] == 'B1',
    'block2 expanded with its own content')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('streaming: bulk runs separated by a whitespace chunk stream into one block', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  -- Bulk load: the whitespace text chunk between the runs is held in
  -- pending_whitespace (no collapse), so the second run streams into the same
  -- expanded block (and the held whitespace is discarded).
  windowed_render(b, { AssistantMessageStart = {} }, true)
  windowed_render(b, { AssistantThinkingChunk = { content = 'bulk one' } }, true)
  windowed_render(b, { AssistantMessageChunk = { content = '\n' } }, true)
  windowed_render(b, { AssistantThinkingChunk = { content = 'bulk two' } }, true)
  check(is_expanded(), 'bulk run still streaming after the whitespace chunk')
  local block = last_thinking(m)
  check(T.content_of(block, 'content') == 'bulk onebulk two', 'both bulk runs in one block')
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '► [Thinking... press o to collapse]'
    and l[3] == 'bulk onebulk two',
    'chrome row + merged content rendered')
  T.collapse_thinking(m, block, b, ns)
  check(entry_count(b) == 1, 'bulk runs collapsed into a single entry')
  local contents = entry_contents(b)
  check(contents[1] == 'bulk onebulk two', 'merged entry holds both bulk runs')
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]', 'merged run collapsed to one real row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)
