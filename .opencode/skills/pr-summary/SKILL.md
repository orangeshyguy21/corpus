SKILL.md

# pr-summary

Generate a PR title (under 50 chars) and bullet-point summary on request.

## Convention

Use conventional commits: prefix title with `chore:`, `feat:`, `fix:`, `docs:`, `refactor:` etc.
Title must be under 50 characters including the prefix.

## Output format

Always return two code-blocked sections (title and summary), each copyable on its own:

```markdown
## Title

```
<pr title>
```

## Summary

- bullet one
- bullet two
- ...
```
