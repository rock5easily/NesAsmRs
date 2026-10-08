# Compatibility fixtures

Inputs for `compare.py` and the `golden` / `assembly` Rust tests.

- `AdditionalDirectiveTest/`, `AdditionalFunctionTest/`: copies of the C# test
  data (`Tests/*/TestData` in NesAsmSharp). Case names are `<suite>_<file>`.
- `general/`: fixtures added for the Rust port. Case names are the file stems.
