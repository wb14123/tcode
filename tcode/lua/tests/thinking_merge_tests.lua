-- Merge behavior for consecutive thinking runs with nothing visible between
-- them. One continuous reasoning stream (DeepSeek bursts via OpenRouter,
-- Claude back-to-back thinking blocks) can be split into multiple collapsed
-- entries when the 500ms settle flush fires during a pause; the next run must
-- merge into the previous entry instead of creating a second one. When any
-- real content (text, tool labels, subagent sections, an expanded entry) sits
-- between the runs, they must stay separate.
--
-- A merged (reopened) block renders its FULL content — the old tail-only
-- render dropped the previous runs from the display. Buffer assertions use
-- the integer row map and real lines, never extmarks.
--
-- Entries are inspected via the MODEL: a thinking "entry" is a thinking block
-- that is not currently open (state collapsed or expanded).

local function row_of(m, el)
  return T.get_renderer_state(m).rows[el.id]
end

local function entry_count(b)
  local n = 0
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' and el.state ~= 'open' then n = n + 1 end
  end
  return n
end

local function entry_contents(b)
  local contents = {}
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' and el.state ~= 'open' then
      contents[#contents + 1] = T.content_of(el, 'content')
    end
  end
  table.sort(contents)
  return contents
end

local function is_open_thinking()
  local tail = T.model.tail
  return tail and tail.type == 'thinking_block' and tail.state == 'open'
end

local function last_thinking(m)
  local el = nil
  for _, e in ipairs(m.elements) do
    if e.type == 'thinking_block' then el = e end
  end
  return el
end

test('merge: runs split by a pause collapse into a single entry', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { 'label', '' })
  -- Run 1 streams live, then the settle flush collapses it.
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1\nA2' } }, false)
  check(is_open_thinking(), 'run 1 open')
  local block = last_thinking(m)
  T.collapse_thinking(m, block, b, ns)
  check(not is_open_thinking(), 'run 1 collapsed by the flush')
  -- Run 2 arrives after the pause: the collapsed block is still the tail -> merge.
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = '\nB2' } }, false)
  check(is_open_thinking(), 'run 2 merged and streaming')
  -- The reopen renders the FULL merged content (the old tail-only render lost
  -- the previous run — bug 1 root cause C).
  local l = lines_of(b)
  check(l[1] == 'label', 'label row intact')
  check(l[3] == 'A1' and l[4] == 'A2B1' and l[5] == 'B2',
    'merged reopen streams the full content, previous run included')
  local r = row_of(m, block)
  check(r.start_row == 2 and r.height == 3, 'row map: merged block at [2, 5)')
  -- Final collapse yields ONE entry with the combined text.
  T.collapse_thinking(m, block, b, ns)
  check(entry_count(b) == 1, 'single thinking entry after the merged collapse')
  local contents = entry_contents(b)
  check(contents[1] == 'A1\nA2B1\nB2', 'entry holds both runs in order')
  l = lines_of(b)
  check(l[1] == 'label' and l[2] == '' and l[3] == '► [Thinking... press o to expand]',
    'collapsed to one real row below the seeded blank')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('merge: visible text between runs keeps separate entries', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  local block1 = last_thinking(m)
  T.collapse_thinking(m, block1, b, ns)
  -- A real text chunk sits between the thinking runs: no merge.
  windowed_render(b, { AssistantMessageChunk = { content = 'visible text\n' } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(is_open_thinking(), 'second run open as its own entry')
  local block2 = last_thinking(m)
  T.collapse_thinking(m, block2, b, ns)
  check(entry_count(b) == 2, 'two separate entries kept')
  local contents = entry_contents(b)
  check(contents[1] == 'A1' and contents[2] == 'B1', 'entries hold their own content')
  local l = lines_of(b)
  -- block1 is ATTACHED inside the am, so it renders above the visible text
  -- (arrival order); block2 sits after the text as its own attached block.
  check(l[2] == '► [Thinking... press o to expand]' and l[3] == 'visible text'
    and l[5] == '► [Thinking... press o to expand]',
    'attached block1 hint, visible text, block2 hint in arrival order')
  check(row_of(m, block1).start_row == 1 and row_of(m, block1).height == 1
    and row_of(m, block2).start_row == 4 and row_of(m, block2).height == 1,
    'row map: block1 attached at [1, 2) inside the am, block2 at [4, 5)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('merge: a tool call between runs keeps separate entries', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  local block1 = last_thinking(m)
  T.collapse_thinking(m, block1, b, ns)
  -- A tool call starts between the thinking runs: no merge.
  windowed_render(b, { AssistantToolCallStart = { tool_name = 'bash', tool_call_id = 'tc1', tool_call_index = 1 } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(is_open_thinking(), 'second run open as its own entry')
  local block2 = last_thinking(m)
  T.collapse_thinking(m, block2, b, ns)
  check(entry_count(b) == 2, 'two separate entries kept (one per thinking phase)')
  local l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]' and l[4] == '► [Thinking... press o to expand]',
    'two one-row collapsed entries around the tool region')
  check(l[3]:find('TOOL', 1, true) ~= nil, 'tool label preserved between the indicators')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('merge: an expanded previous entry is never merged', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  local block1 = last_thinking(m)
  T.collapse_thinking(m, block1, b, ns)
  -- The user expanded the entry to read it: the merge guard must keep it
  -- separate from a later run.
  T.toggle_thinking(m, block1, b, ns)
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(is_open_thinking(), 'new run opened without merging')
  local block2 = last_thinking(m)
  T.collapse_thinking(m, block2, b, ns)
  check(entry_count(b) == 2, 'two separate entries kept')
  -- The expanded block is never merged: it stays expanded and keeps its own
  -- content in the model (the merge guard is model-level).
  local blocks = {}
  for _, el in ipairs(m.elements) do
    if el.type == 'thinking_block' then blocks[#blocks + 1] = el end
  end
  check(#blocks == 2 and blocks[1].state == 'expanded', 'expanded entry untouched by the new run')
  check(T.content_of(blocks[1], 'content') == 'A1' and T.content_of(blocks[2], 'content') == 'B1', 'both entries hold their own content')
  local contents = entry_contents(b)
  check(contents[1] == 'A1' and contents[2] == 'B1', 'both entries hold their own content (sorted)')
  local l = lines_of(b)
  check(l[2] == '► [Thinking... press o to collapse]' and l[3] == 'A1',
    'expanded entry still visible on screen')
  check(l[4] == '► [Thinking... press o to expand]', 'new run collapsed to its own one-row indicator')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('merge: bulk runs separated by a whitespace text chunk merge', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  -- Bulk load: the whitespace text chunk between the runs triggers a collapse,
  -- and the deferred second run must merge back into the same entry.
  windowed_render(b, { AssistantMessageStart = {} }, true)
  windowed_render(b, { AssistantThinkingChunk = { content = 'bulk one' } }, true)
  windowed_render(b, { AssistantMessageChunk = { content = '\n' } }, true)
  windowed_render(b, { AssistantThinkingChunk = { content = 'bulk two' } }, true)
  check(is_open_thinking(), 'bulk run open after the whitespace collapse')
  local block = last_thinking(m)
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'bulk onebulk two',
    'merge reopen renders the full merged content')
  T.collapse_thinking(m, block, b, ns)
  check(entry_count(b) == 1, 'bulk runs merged into a single entry')
  local contents = entry_contents(b)
  check(contents[1] == 'bulk onebulk two', 'merged entry holds both bulk runs')
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]', 'merged run collapsed to one real row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)
