# Known Issues / TODOs

Deferred / known problems we're consciously living with — not yet worth their own
spec step. Each row is a **`KI-NNNN`** (zero-padded to 4 so ids sort correctly).

Conventions:
- **Add** a row when you *decide to defer* something real — not for every papercut.
- Trivial, code-local stuff stays as an inline `// TODO` / `// FIXME` instead.
- When an issue lives at a spot in the code, drop `// TODO(KI-NNNN): …` there so
  code ↔ ledger is one grep (`rg -n 'KI-[0-9]{4}'`).
- **Delete** a row when fixed (git log is the history); **promote** to a spec
  when it grows teeth.
- **This ledger is live state; `specs/` is the frozen decision record.** A spec's
  Deferred section says what we chose not to build *then*; a row here says what is
  wrong *now*. If never doing it would leave something **wrong**, it belongs here;
  if it would leave something **missing** (a feature not yet built), it belongs in
  a spec's Deferred list, not here.

| ID | Area | Severity | Status | Issue / notes |
|----|------|----------|--------|---------------|
