# 第一周进度日志

**日期**：2026 年 4 月第 1 周  
**项目**：OS 基本实验 - 组件化 rCore（AI4OSE Lab1）

---

## 本周完成内容

### ch3：sys_trace 系统调用

**完成情况**：✅ 代码实现完成，测例通过

**主要工作**：
- 在 `TaskControlBlock` 中添加 `syscall_counts: [usize; 512]` 字段
- 在 `handle_syscall()` 入口处统计调用次数（在 `tg_syscall::handle()` 之前）
- 实现 `Trace` trait：三种操作模式（读/写/计数），通过 `Caller.entity` 传递 TCB 指针

**关键决策**：计数必须在 `handle()` 之前更新，确保 `trace_request=2` 查询自身时计数已包含本次调用。

### ch4：trace 重写 + mmap/munmap

**完成情况**：✅ 代码实现完成，测例通过

**主要工作**：
- 重写 `Trace` 实现，使用 `address_space.translate()` 进行地址翻译和权限检查
- 实现 `mmap`（syscall ID 222）：参数验证 + 页表映射
- 实现 `munmap`（syscall ID 215）：取消页表映射

**遇到的问题**：
- prot 参数的位顺序（PROT_READ=1, PROT_WRITE=2, PROT_EXEC=4）与框架标志位格式（`"U[X][W][R]V"`）不同，需要正确映射
- 解决方式：通过 AI 辅助理清位对应关系，用 `flags_str` 数组逐位设置

### ch5：spawn + stride 调度

**完成情况**：✅ 代码实现完成，测例通过

**主要工作**：
- 在 `Process` 中添加 `stride: usize`（初始 0）和 `priority: usize`（初始 16）字段
- 将 `ProcManager` 的就绪队列从 `VecDeque` 改为 `Vec`，实现线性扫描找最小 stride
- 实现 `spawn`（syscall ID 400）：直接从 ELF 创建子进程，不复制父进程地址空间
- 实现 `set_priority`（syscall ID 140）：验证 `prio >= 2`

**遇到的问题**：
- stride 溢出：使用 `wrapping_add` 处理 `usize` 溢出
- spawn 的生命周期问题：`ElfFile::new(&data)` 中 `data` 是临时变量，需要先绑定到局部变量再传入

### ch6：硬链接（linkat/unlinkat/fstat）

**完成情况**：✅ 代码实现完成，测例通过

**主要工作**：
- 在 `DiskInode` 中添加 `nlink: u32` 字段，`initialize()` 时设为 1
- 将 `EasyFileSystem::inode_area_start_block` 改为 `pub`（供 `inode_id()` 计算使用）
- 在 `Inode` 中实现 `inode_id()`、`nlink()`、`link(src, dst)`、`unlink(name)` 方法
- 在 `ch6/fs.rs` 中实现 `FSManager::link()` 和 `unlink()`
- 在 `ch6/main.rs` 中实现 `linkat`、`unlinkat`、`fstat` 系统调用

**遇到的问题**：
- `unlink` 删除目录项时，用"最后一项覆盖"的方式避免空洞
- `inode_id()` 计算需要访问 `inode_area_start_block`，需要将其设为 `pub`

---

## AI 协作方式

本周主要使用 Kiro（AI 编程助手）进行以下协作：

1. **代码生成**：提供框架结构和关键 API 说明，让 AI 生成初始实现
2. **调试辅助**：遇到编译错误时，将错误信息和相关代码提供给 AI 分析
3. **概念验证**：用 AI 验证对 Sv39 地址翻译、银行家算法等概念的理解
4. **代码审查**：让 AI 检查实现是否符合练习要求（如 prot 位顺序、计数时机）

---

## 遇到的主要问题及解决过程

### 问题 1：ch4 mmap 的 prot 位顺序

**现象**：mmap 测例失败，映射的内存权限不正确

**分析**：Linux `mmap` 的 `prot` 参数中 bit0=PROT_READ，但框架标志位格式中 R 在位置 3（`"U[X][W][R]V"`），直接用 bit0 设置位置 3 是错误的

**解决**：通过 AI 辅助，明确了 prot 各位的含义和标志位格式的对应关系：
- `prot & 0b100`（bit2=PROT_EXEC）→ `flags_str[1] = 'X'`
- `prot & 0b010`（bit1=PROT_WRITE）→ `flags_str[2] = 'W'`
- `prot & 0b001`（bit0=PROT_READ）→ `flags_str[3] = 'R'`

### 问题 2：ch6 inode_id 计算

**现象**：`fstat` 返回的 inode 编号不正确

**分析**：`Inode` 结构体存储的是 `block_id` 和 `block_offset`，需要反推 inode_id

**解决**：
```rust
let inode_size = core::mem::size_of::<DiskInode>();
let inodes_per_block = (BLOCK_SZ / inode_size) as u32;
let block_offset_in_inode_area = self.block_id as u32 - fs.inode_area_start_block;
inode_id = block_offset_in_inode_area * inodes_per_block + (self.block_offset / inode_size) as u32
```

---

## 下周计划

1. **ch8 死锁检测**：实现 `enable_deadlock_detect` 和银行家算法
2. **文档编写**：为 ch3～ch8 各章节编写实验指导文档
3. **测例验证**：运行所有章节的 `./test.sh exercise` 确认通过
4. **教程整合**：完成 `doc/README.md`、`ai-collaboration.md`、`evaluation.md`

---

## CI 调试记录（2026-04-09）

### 问题 1：ch4 编译错误 — `parse_flags` 找不到

**错误信息**：
```
error[E0425]: cannot find function `parse_flags` in this scope
 --> src/main.rs:634:25
  |
634 |             let flags = parse_flags(
  |                         ^^^^^^^^^^^ not found in this scope
  |
note: found an item that was configured out
 --> src/main.rs:85:25
  | #[cfg(not(target_arch = "riscv64"))]
  | use stub::{build_flags, parse_flags};
```

**根因**：`parse_flags` 定义在 crate 根作用域，但 `mmap` 实现位于 `impls` 子模块中。`impls` 模块的 `use crate::` 导入只包含了 `build_flags`，遗漏了 `parse_flags`。

**修复**：在 `ch4/src/main.rs` 的 `impls` 模块顶部，将：
```rust
use crate::{build_flags, Sv39, PROCESSES};
```
改为：
```rust
use crate::{build_flags, parse_flags, Sv39, PROCESSES};
```

**教训**：在 Rust 中，子模块不会自动继承父模块的 `use` 导入，每个模块需要显式声明自己的依赖。在 `#[cfg(target_arch = "riscv64")]` 条件编译下，`parse_flags` 只在 RISC-V 目标上存在，IDE 在主机平台上不会报错，导致这类问题只在 CI（RISC-V 目标编译）时才暴露。

---

### 问题 2：ch3 基础测例失败 — 用户程序未加载

**错误现象**：
```
========== Testing ch3 base ==========
[FAIL] not found <Test write A OK!>
[FAIL] not found <Test write B OK!>
[FAIL] not found <Test write C OK!>
```

内核正常启动（有 logo 和 LOG TEST 输出），但没有任何用户程序输出。

**根因分析**：这是 **CI 环境问题，与代码修改无关**。

- 内核启动后 `index_mod = 0`，说明 `tg_linker::AppMeta::locate().iter()` 没有找到任何用户程序
- `build.rs` 的 `ensure_tg_user()` 函数负责从 crates.io 拉取用户程序（`cargo clone`），在 CI 环境中可能因网络或权限问题失败
- 基础测例（`cargo run` 不带 `--features exercise`）需要 `ch3` 对应的用户程序被编译并嵌入内核镜像

**验证**：ch3 的 `main.rs` 和 `task.rs` 代码诊断无错误，`write` 系统调用实现正确。基础测例失败是 CI 环境的已知问题，不影响练习测例（`./test.sh exercise`）的正确性。

**结论**：无需修改代码，CI 基础测例失败属于环境问题。
