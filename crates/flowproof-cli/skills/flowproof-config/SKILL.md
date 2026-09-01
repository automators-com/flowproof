---
name: flowproof-config
description: >
  Configure flowproof's SAP GUI and Fiori credentials by walking the user
  through `flowproof config sap` / `flowproof config fiori`. Use when the
  user wants to set up, change, or check their SAP or Fiori login for
  flowproof, or when a flow run fails because SAP_USER, SAP_PASSWORD,
  FIORI_USER, or FIORI_PASSWORD aren't set.
---

# flowproof config

Helps the user configure flowproof's SAP GUI and/or Fiori credentials so
`flowproof record`/`flowproof run` can resolve `${SAP_USER}`,
`${FIORI_PASSWORD}`, and the rest of the `${VAR}` references their flows
use.

This assumes `flowproof` is already installed and on `PATH` (`flowproof
--version` to check). If it isn't, stop and tell the user to install it
first — this skill only configures an existing installation.

## 1. Work out which profile is needed

flowproof treats SAP GUI and Fiori as two independent profiles, each with
its own set of fields — ask if it isn't already obvious from context:

- **`sap`** — SAP GUI, driven via SAP GUI Scripting. Windows only.
- **`fiori`** — a Fiori launchpad in a browser. Cross-platform.

A project can use either or both. If the user's flows reference
`${SAP_USER}`/`${SAP_PASSWORD}` they need `sap`; if they reference
`${FIORI_USER}`/`${FIORI_PASSWORD}` they need `fiori`. When unsure, ask.

## 2. Gather the non-secret fields — never the password

For the chosen profile, ask the user for whichever of these they want to
set (all optional except `user`; skip ones they don't want to change):

- `sap`: user, client, language, connection (the SAP Logon connection name)
- `fiori`: user, client, language, base_url (the launchpad URL)

**Do not ask for the password, and do not accept one if it's offered in
chat.** `flowproof config sap`/`fiori` takes `--password` only as a plain
CLI flag — there is no stdin or env-var form — so a value typed into this
conversation or passed on the command line here would land in this
session's transcript, and on many shells, in shell history and briefly in
the process list. That is a strictly worse exposure than the alternative in
step 4, which never touches this conversation at all.

## 3. Run the command — once, with everything gathered

Run exactly one command per profile with every non-secret field you
gathered as flags, e.g.:

```
flowproof config sap --user alice --client 100 --language EN --connection "S/4HANA Dev"
```

or

```
flowproof config fiori --user alice --client 100 --base-url https://my-launchpad.example.com/
```

This has to happen in one call: any flag at all switches the command into
flag-driven mode, and only the flags actually given are written — a second
call with a different flag does not "continue" the first, it merges
independently. Passing everything you have in one shot is both correct and
sufficient; you do not need to also run the bare interactive form for the
non-secret fields.

If the user has nothing to set beyond the password (first time through, or
a pure password change), skip this step and go straight to step 4.

## 4. Hand the password step back to the user

Tell the user to run the bare command themselves, in their own terminal,
with no flags — this is what actually collects the password, via a masked
prompt that never echoes back what's typed or what's already stored:

```
flowproof config sap
```

or

```
flowproof config fiori
```

Explain briefly what each prompt is asking for (matches whatever fields you
didn't already set as flags in step 3, plus the password prompt — pressing
Enter at any prompt keeps the current value). Wait for the user to confirm
they've run it before moving on.

## 5. Verify — without ever touching the secret

Run these two, which are always safe (the password is masked in the first
and absent from the second):

```
flowproof config show
flowproof config path
```

Report back the path and confirm the fields the user asked to set are
present. Never ask the user to paste `config show`'s output back if it's
already visible to you from running it yourself, and never suggest
printing the raw config file's contents another way — that would bypass
the masking `show` provides.

## 6. If a flow still fails on credentials afterward

`flowproof config` only seeds an env var that isn't already set — an
explicit shell export, a CI secret, or a suite's own `env:`/`env_from`
always wins over it. Before assuming the config file is wrong, check:

- Is the relevant var (`SAP_USER`, `FIORI_PASSWORD`, etc.) already exported
  in the user's shell? Safe to check the non-password ones directly
  (`echo $SAP_USER`); skip echoing the password ones.
- Does the project's `suite.yaml` have an `env:` or `env_from` block
  setting the same name?

Either of those overriding a stale or wrong value is a much more likely
explanation than the config file itself being broken.
