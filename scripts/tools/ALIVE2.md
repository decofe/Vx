# Checking Vx LLVM IR with Alive2

This opt-in tool compares selected functions in two LLVM IR modules. It can
validate a candidate transformation, but it does **not** prove Vx source
semantics, borrow checking, cross-function behavior, or device-code correctness.
Alive2 has bounded reasoning for loops and can return inconclusive results.

Build the standalone harness with `rustc --edition=2021 scripts/tools/alive2.rs
-o vx-alive2`. Build `vxc` separately, and install a compatible
`mlir-translate`, `opt`, and `alive-tv` (see [Alive2's installation and
limitations](https://github.com/AliveToolkit/alive2#readme)). Vx prints
LLVM-dialect MLIR with `--emit-llvm` when no concrete host target is selected;
the harness translates that output into actual LLVM IR before invoking Alive2.
Set `VXC`, `MLIR_TRANSLATE_PATH`, `OPT_PATH`, or `ALIVE_TV` to override executable
locations. Use the same LLVM version for the translator, optimizer, and Alive2.

For a small, self-contained Vx file with a scalar `probe` function, try:

```sh
./vx-alive2 --mode codegen --function probe --out /tmp/vx-codegen-proof example.vx
./vx-alive2 --mode mlir --function probe --out /tmp/vx-mlir-proof example.vx
./vx-alive2 --mode llvm --function probe --out /tmp/vx-llvm-proof example.vx
./vx-alive2 --mode reference --reference specification.ll --function probe \
  --out /tmp/vx-spec-proof example.vx
```

`codegen` compares `--legacy-codegen -O0` with default codegen `-O0` in both
directions after applying LLVM's `mem2reg` to **both** modules. Both raw and
normalized IR remain in the output directory. This avoids spurious Alive2
counterexamples for equivalent phi-node and stack-slot lowering (observed on
`tests/backend/pass/expr_assignment.vx`). `mlir` checks whether default codegen
`-O3` refines `-O0`.
`llvm` checks a small LLVM optimization pipeline against default `-O0` IR.
`reference` checks whether Vx `-O0` refines the supplied reference IR. The
reference must define the same named function with a compatible LLVM ABI; it
must encode the intended behavior independently of Vx's output. Functions
calling other functions are **not** checked end-to-end by this comparison.
Avoid comparisons involving changed ABIs or target-specific module settings.

The output directory must not already exist. The harness stores the Vx input,
both IR modules, commands, stdout, and stderr there for reproducing a result.
Repeat `--function` to check multiple functions. Exit status `0` means a
nonempty Alive2 proof for every selected function, `1` means at least one
counterexample candidate, and `2` means a tool failure or inconclusive result.
Inspect each `alive2-*.stdout` and `alive2-*.stderr` before filing a bug:
undefined behavior in the source, external calls, unsupported constructs,
and incomplete loop reasoning can invalidate an apparent counterexample.
Include the saved IR and commands with a reduced reproducer.
