SKILL.md

# commit-summary

Generate a commit title (under 50 chars) and summary for a branch/diff on request.

## Convention

Use conventional commits: prefix title with `chore:`, `feat:`, `fix:`, `docs:`, `refactor:`, etc.

Title must be under 50 characters including the prefix.

## Workflow

1. Determine commit type by inspecting `git diff --stat HEAD` and branch name
2. Generate concise title (under 50 chars) following conventional commits
3. Produce a short summary paragraph (3-4 sentences max) describing what the changes do
