# Board recipes

Exact `gh api` calls for the ai-team board on GitHub Issues. They use REST
only, so they work where `gh issue` is blocked. Run them inside the repo's
checkout: `gh` fills in `{owner}` and `{repo}` from the git remote.

GitHub links sub-issues and blocked-by edges by an issue's **database id**,
not its `#number`; get it with the second command.

```bash
# Check the target repo before the first write
gh api repos/{owner}/{repo} --jq .full_name

# Database id of issue N
gh api repos/{owner}/{repo}/issues/N --jq .id

# Create a label once (an "already_exists" error is fine)
gh api repos/{owner}/{repo}/labels -f name=ai-team:engineer -f color=0e8a16

# Create an issue; prints its number. For a long body use -F body=@file.md.
# A new issue starts in the Backlog status.
gh api repos/{owner}/{repo}/issues -f title="…" -f body="…" \
  -f "labels[]=ai-team:engineer" --jq .number

# Read an issue, and its comments
gh api repos/{owner}/{repo}/issues/N --jq '{title, state, labels: [.labels[].name], body}'
gh api "repos/{owner}/{repo}/issues/N/comments?per_page=100" --jq '.[] | {user: .user.login, body}'

# Make C a sub-issue of epic P, and list P's sub-issues
gh api -X POST repos/{owner}/{repo}/issues/P/sub_issues -F sub_issue_id=<database id of C>
gh api "repos/{owner}/{repo}/issues/P/sub_issues?per_page=100" \
  --jq '.[] | {number, title, state, labels: [.labels[].name]}'

# C is blocked by B; count N's open blockers (0 means it can start)
gh api -X POST repos/{owner}/{repo}/issues/C/dependencies/blocked_by -F issue_id=<database id of B>
gh api repos/{owner}/{repo}/issues/N --jq .issue_dependencies_summary.blocked_by

# Comment, edit the body (e.g. the epic), close
gh api repos/{owner}/{repo}/issues/N/comments -F body=@report.md
gh api -X PATCH repos/{owner}/{repo}/issues/N -F body=@epic.md
gh api -X PATCH repos/{owner}/{repo}/issues/N -f state=closed

# Create the question label once; a question issue also carries ai-team:human
gh api repos/{owner}/{repo}/labels -f name=ai-team:question -f color=d93f0b
gh api repos/{owner}/{repo}/labels -f name=ai-team:human -f color=fbca04

# Assign to the human (the account gh is logged in as)
gh api repos/{owner}/{repo}/issues/N/assignees -f "assignees[]=$(gh api user --jq .login)"

# Count the team's open PRs waiting on the human
gh api "repos/{owner}/{repo}/pulls?state=open&per_page=100" \
  --jq '[.[] | select((.body // "") | test("Closes #"))] | length'
```

## Status

Status is GitHub's own issue status, not a label. It is GraphQL only (REST
doesn't show it): Backlog, Todo, In progress, In review, Done, Not planned,
Duplicate. Meaning on the board:

| Status | Meaning |
|--------|---------|
| Backlog | Planned, not yet approved (an issue with no status counts as Backlog) |
| Todo | Approved; the manager may assign it |
| In progress | An employee is working on it |
| In review | Its PR is open |
| Done | Merged; setting it closes the issue |
| Not planned | Dropped; closes the issue |

Blocked is not a status: a task with open blocked-by links is blocked. Setting
Todo, In progress or In review reopens a closed issue.

```bash
# Read: status name (null or Backlog = Backlog), state
gh api graphql -f query='query{repository(owner:"{owner}",name:"{repo}"){issue(number:N){state status{name}}}}' \
  --jq '.data.repository.issue'

# Status ids differ per repo: look them up, then set one by id
gh api graphql -f query='query{repository(owner:"{owner}",name:"{repo}"){planningStatuses(first:20){nodes{id name}}}}' \
  --jq '.data.repository.planningStatuses.nodes[]'
gh api graphql -f query='mutation($i:ID!,$s:ID!){setIssueStatus(input:{issueId:$i,statusId:$s}){clientMutationId}}' \
  -f i="$(gh api repos/{owner}/{repo}/issues/N --jq .node_id)" -f s="<status id>"
```

This is a GitHub preview feature. If these calls fail (the status is missing or
can't be set), tell the human, and fall back to a `Status:` line in a comment
on the task.

## A question for the human

A PM never asks the human directly. It files a sub-issue of the epic (a
standalone issue if no epic exists yet) labelled
`ai-team:human` and `ai-team:question`, assigned to the human, then keeps
working on the tasks the question doesn't affect:

```markdown
> *Posted by the ai-team manager.*

## Question
Should the export include archived projects?

## Options
1. **Exclude archived (Recommended)**: smaller files, matches the UI; adds a
   flag later if asked.
2. Include archived: complete, but doubles file size for large accounts.

## Parked
#12 (export format) waits on this. #13 and #14 keep going.

Answer by commenting `Answer: …` here and closing the issue.
```

The human comments `Answer: <option label> — <note>` (or `Answer: Let the PM
decide`) and closes the issue. The PM reads it at its next check-in. On `Answer: Let the PM decide` the PM decides within scope and
logs `Decision: <what> — human left it to the PM`. The first line of `## Parked`
goes into the phone ping, so keep it short.

## A friction issue

When the way the team works cost real time or tokens, a PM files one issue in
its own repo labelled `ai-team:friction` (create the label on first use). It is
not assigned to the human: the human reads these on the board and decides
which become Skill changes. Keep it short and free of
secrets (the repo may be public):

```markdown
> *Posted by the ai-team manager.*

**What happened:** reviewer re-read the whole `src/` on every fix round.
**Evidence:** #14, #15 (3 rounds each); Usage line on the epic: 410k tokens.
**Skill or rule involved:** reviewer, "scoped re-review".
**Cost:** about 2x the tokens of similar tasks.
```

## Without a GitHub remote

Keep the same board as files in the main checkout, and add `.team/` to
`.git/info/exclude` so it's never committed:

- `.team/epic-<slug>.md`: one epic per feature, holding its body.
- `.team/tasks/<n>-<slug>.md`: one task or decision ticket per file, numbered
  in creation order, starting with a `Labels:`, `Status:`, `Part of:`, and
  `Blocked by:` line (`Status:` takes the values above); reports and answers are appended under `## Report (<role>)`.
