**Keyboard input reaches a Citrix or RDP session.** A remote-display client
forwards keyboard input by scan code, and flowproof sent none: key presses
went out with a scan code of 0 and typed text as Unicode packets, which carry
no key at all - so in a Citrix session typing and keys could simply vanish.
Every injected key now carries its real scan code, with the extended flag on
arrows and navigation keys so they are not read as their keypad twins, and
`app: vision` types text as the key strokes a keyboard would make, falling
back to a Unicode packet only for a character no plain key produces. Local
apps read the same virtual keys as before.
