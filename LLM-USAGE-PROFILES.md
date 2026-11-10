# Profile support in llm-usage

Users with separate personal and work accounts need to fetch subscription quotas
for either account without switching their active login.

Add a way to select a profile or credential directory when fetching usage. The
selected profile should determine which credentials are used, while keeping the
existing quota output format and provider filtering.

Profile selection should work with both one-shot JSON output and periodic file
refresh, so separate profiles can be collected independently.

Do not change the active login, copy credentials between profiles, or fall back
to another account when the selected profile is unavailable. Report missing or
invalid credentials clearly. Existing commands without an explicit profile
should continue to work as before.
