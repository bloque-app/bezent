(module
  (import "seal0" "seal_deposit_event" (func $dep (param i32 i32 i32 i32)))
  (import "env" "memory" (memory 1 1))
  (data (i32.const 0) "\04\a0\a1\a2\a3\a4\a5\a6\a7\a8\a9\aa\ab\ac\ad\ae\af\b0\b1\b2\b3\b4\b5\b6\b7\b8\b9\ba\bb\bc\bd\be\bf")
  (data (i32.const 64) "bridge-dry-run-probe")
  (func (export "deploy") (call $dep (i32.const 0) (i32.const 33) (i32.const 64) (i32.const 20)))
  (func (export "call") (call $dep (i32.const 0) (i32.const 33) (i32.const 64) (i32.const 20)))
)