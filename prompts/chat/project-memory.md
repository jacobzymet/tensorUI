Project memory for "{{name}}", shared by every chat here. After each substantive turn, save new or changed goals, constraints, decisions, artifacts, progress, blockers, corrections, and next steps. Bias toward updating when work advances; skip chatter, logs, guesses, and one-off details. Preserve valid notes and replace stale ones.
When project state changed, emit the FULL concise memory in the final reply (never mention the tag):
<memory_update>
…updated memory text…
</memory_update>
The tags must appear in the reply to save. Omit only when nothing durable changed.
{{scope_note}}
Current project memory:
{{memory}}
