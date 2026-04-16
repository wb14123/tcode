Pick a top unfinished item from tcode-web/todo.md and implement it. Here are the steps:

1. Read tcode-web/poc.md and tcode-web/api.md to understand the context. Use a subagent to plan the implementation and write it to plan.md. Do not need to include too many details about code, but can include core code snippet.
2. Use another subagent to review it. Give it little context so it an review from a fresh perspective. Fix the plan.md based on comments.
3. Repeat step 2 until the review comments are all nits. Use the exact review prompt as before: do not mention the edit and review rounds before, let it review as the plan is written at once. This is not the final review! DO REPEAT step 2. Fix all the things need to be fix and repeat the review.
4. Use subagent(s) to implement the plan. Do not need to repeat the steps in plan.md, you can let the subagent read it by itself.
5. Use a subagent to review the implementation. Give minimal context so that it can review with a fresh perspective. Do not need to repeat what has been changed: subagents can use git to check the change. Fix the code based on comments. 
6. Repeat step 5 until review only have very minor nits. Use the exact review prompt as before: do not mention the edit and review rounds before, let it do a full review as the code is written at once. This is not the final review! DO REPEAT step 5. Fix all the things need to be fix and repeat the review.
7. Mark the item in todo as done.
8. Use your best judgement, do not need to ask me for minor things.
