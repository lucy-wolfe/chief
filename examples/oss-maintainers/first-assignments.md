# First assignments — Patchwork Labs

Hand these to the CEO, one at a time. The first one needs a repository — name
one you actually care about.

## 1. Triage the open issues

> The repository is `<owner>/<repo>`. Ask Triage to read its open issues and
> classify every one: bug, support question, feature request, or duplicate. For
> each of the top ten by impact, the Triage Lead writes the reply a maintainer
> could post unedited. Bring me the list ranked, not the raw dump.

Watch for: the Issue Analyst being handed the ambiguous ones to reproduce
instead of the Triage Lead guessing.

## 2. A fix plan for the top bug

> Take the highest-impact bug from that list to Engineering. I want the plan
> before the patch: the cause, the smallest change that addresses it, the risk,
> and the test that would have caught it. Engineering Lead signs the plan, then
> the engineers produce the failing test first and the patch second.

Watch for: the Engineering Lead refusing a patch with no test. That refusal is
written into the charter as an always, and you should see it enforced.

## 3. Draft the release notes

> Ask Release to draft notes for the work the company has produced so far.
> Grouped by impact, breaking changes first, in the maintainer's voice — and
> nothing in them for work Engineering never signed off.

Watch for: the Release Manager coming back with a shorter document than you
expected, because it dropped the unsigned work. That is the mandate holding.
