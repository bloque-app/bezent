(module
  (import "seal0" "seal_deposit_event" (func $dep (param i32 i32 i32 i32)))
  (import "env" "memory" (memory 1 1))
  (data (i32.const 0) "\00")
  (data (i32.const 64) "bridge-dry-run-probe")
  (func (export "deploy") (call $dep (i32.const 0) (i32.const 1) (i32.const 64) (i32.const 20)))
  (func (export "call") (call $dep (i32.const 0) (i32.const 1) (i32.const 64) (i32.const 20)))
)