**Recording a vision step finds a field below the fold before checking it is
there.** The recorder checks that a step's target exists before acting, and
for a pixels-only surface that check sees only the visible window - so once
vision steps were performed straight from the grammar, every field further
down a form was "not found" before anything scrolled. Recording now brings the
target into view first, as replay already does; nothing changes for other
apps.
