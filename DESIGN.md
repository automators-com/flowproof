---
extends:
  repository: automators-com/design-system
  version: "1.0.1"
packages:
  tokens: "@automators-com/design-tokens@1.0.1"
profile: product-application
---

# Flowproof design profile

Flowproof extends the Automators brand foundation for its product interface,
documentation, reports, and presentation decks.

The presentation sources under `docs/slides/` use the pinned foundation colors,
Noto Sans for reading text, JetBrains Mono for specifications and readouts, and
the foundation spacing and corner tiers. Slide composition and technical content
remain owned by this repository.

## Presentation-specific decisions

- Slides use a 16:9, 1280 x 720 canvas.
- Projected body copy stays at 16 px or larger. Dense specification readouts may
  use 13 px, the foundation's documented readout floor.
- Page numbers and version labels are functional readouts, not decorative small
  headers.
- Code cards are raised Paper surfaces with no decorative border.
- The single mint signal identifies a verified outcome or current execution
  state; purple carries identity and emphasis.

## Migration status

The training and solutions decks were rebuilt as native editable presentations
for Flowproof 0.12.2. The former PDF-overlay generators are not canonical
sources.
