---
summary: When to stay in this chat's checkout and when to make a worktree.
---
Stay in this chat's own checkout. Create a worktree only when the checkout has uncommitted changes you did not make or another chat is working in it, and only if app_worktree_status says worktrees are allowed.

When worktrees are off and the checkout is busy, do not work around it: name a free checkout if the status lists one, tell the user, and stop.

Put new worktrees under worktreeRoot as the status's suggestedPath describes, with a name of lowercase letters, digits and hyphens (at most 32 characters). Never remove a worktree you did not make or whose work is not merged.
