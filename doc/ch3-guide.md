# ch3 实验指导：多道程序与 sys_trace

## 1. 背景与原理

### 1.1 多道程序与分时系统

**多道程序**（Multiprogramming）是指多个用户程序同时驻留在内存中，内核在它们之间切换执行。与批处理系统（串行执行）相比，多道程序能更充分地利用 CPU 资源。

**分时系统**（Time-sharing）在多道程序基础上引入时钟中断，强制每个程序只能运行固定时间片（time slice），到期后切换到下一个程序，实现"看起来同时运行"的效果。

ch3 的调度策略是**时间片轮转（Round-Robin）**：

```
任务 0 → 任务 1 → 任务 2 → 任务 0 → ...
         ↑ 时钟中断触发切换
```

### 1.2 TaskControlBlock 数据结构

`TaskControlBlock`（TCB）是 ch3 的核心数据结构，管理单个任务的全部状态：

```rust
pub struct TaskControlBlock {
    ctx: LocalContext,                    // 用户态寄存器上下文（pc、sp、a0-a7 等）
    pub finish: bool,                     // 任务是否已完成
    stack: [usize; 1024],                 // 8 KiB 用户栈（内嵌在 TCB 中）
    pub syscall_counts: [usize; 512],     // 系统调用计数表，索引 = syscall ID
}
```

关键设计点：
- **用户栈内嵌**：ch3 没有地址空间，用户栈直接分配在内核的 TCB 结构体内
- **计数表大小**：覆盖 0..511 的所有 syscall ID，用 `usize` 存储调用次数
- **`LocalContext`**：保存用户态寄存器，`execute()` 时恢复并执行 `sret` 进入 U-mode

### 1.3 tg_syscall::Trace trait

`sys_trace`（syscall ID 410）通过 `tg_syscall::Trace` trait 实现：

```rust
pub trait Trace {
    fn trace(&self, caller: Caller, trace_request: usize, id: usize, data: usize) -> isize;
}
```

三种操作模式由 `trace_request` 决定：

| trace_request | 操作 | 返回值 |
|---------------|------|--------|
| 0 | 读取 `id` 地址处一个字节 | 该字节的值（u8 as isize） |
| 1 | 将 `data` 最低字节写入 `id` 地址 | 0 |
| 2 | 查询 syscall ID 为 `id` 的调用次数 | 调用次数 |
| 其他 | 无效请求 | -1 |

### 1.4 Caller 机制

ch3 中 `Caller` 结构体的 `entity` 字段被用来传递 TCB 指针：

```rust
// 在 handle_syscall() 中
tg_syscall::handle(Caller { entity: self as *mut _ as usize, flow: 0 }, id, args)

// 在 Trace 实现中
let tcb = unsafe { &mut *(caller.entity as *mut TaskControlBlock) };
```

这是 ch3 特有的设计——因为没有全局进程表，通过 `Caller.entity` 直接传递 TCB 裸指针。

---

## 2. 实现思路

### 2.1 步骤一：扩展 TaskControlBlock

在 `task.rs` 中为 `TaskControlBlock` 添加 `syscall_counts` 字段：

```rust
pub struct TaskControlBlock {
    ctx: LocalContext,
    pub finish: bool,
    stack: [usize; 1024],
    pub syscall_counts: [usize; 512],  // 新增
}

impl TaskControlBlock {
    pub const ZERO: Self = Self {
        ctx: LocalContext::empty(),
        finish: false,
        stack: [0; 1024],
        syscall_counts: [0; 512],      // 初始化为全零
    };

    pub fn init(&mut self, entry: usize) {
        self.syscall_counts = [0; 512]; // 重置计数
        // ...
    }
}
```

### 2.2 步骤二：在 handle_syscall() 中统计调用次数

**关键时机**：在调用 `tg_syscall::handle()` **之前**更新计数，确保 `trace_request=2` 查询时本次调用已计入：

```rust
pub fn handle_syscall(&mut self) -> SchedulingEvent {
    let id = self.ctx.a(7).into();
    let args = [/* a0..a5 */];

    // 统计系统调用次数（在 handle 之前）
    let id_num: usize = self.ctx.a(7);
    if id_num < 512 {
        self.syscall_counts[id_num] += 1;
    }

    match tg_syscall::handle(Caller { entity: self as *mut _ as usize, flow: 0 }, id, args) {
        // ...
    }
}
```

### 2.3 步骤三：实现 Trace trait

在 `main.rs` 的 `impls` 模块中实现：

```rust
impl Trace for SyscallContext {
    fn trace(&self, caller: Caller, trace_request: usize, id: usize, data: usize) -> isize {
        let tcb = unsafe { &mut *(caller.entity as *mut crate::task::TaskControlBlock) };
        match trace_request {
            0 => unsafe { *(id as *const u8) as isize },   // 直接读取用户内存
            1 => {
                unsafe { *(id as *mut u8) = data as u8 };  // 直接写入用户内存
                0
            }
            2 => {
                if id < 512 { tcb.syscall_counts[id] as isize } else { 0 }
            }
            _ => -1,
        }
    }
}
```

**为什么 ch3 可以直接解引用用户指针？**

ch3 没有地址空间机制，内核与用户程序共享同一物理地址空间（flat memory model）。用户程序的虚拟地址就是物理地址，内核可以直接访问。ch4 引入 Sv39 虚存后，这种直接访问就不再安全，需要通过页表翻译。

---

## 3. AI 协作过程记录

### 3.1 提示词示例

**问题 1：理解 Caller 机制**
```
在 tg-rcore-tutorial-ch3 中，tg_syscall::handle() 的第一个参数是 Caller，
它的 entity 字段是什么？在 Trace 实现中如何用它获取当前 TCB？
```

**问题 2：计数时机**
```
sys_trace 的 trace_request=2 要求"本次调用也计入统计"。
在 handle_syscall() 中，应该在调用 tg_syscall::handle() 之前还是之后更新计数？为什么？
```

### 3.2 关键洞察

通过与 AI 协作，理解了以下关键点：
- `Caller.entity` 是一个通用的"调用者上下文"字段，ch3 中复用它传递 TCB 指针
- 计数必须在 `handle()` 之前更新，否则 `trace(2, 410, 0)` 查询自身调用次数时会少 1
- ch3 的 `unsafe` 直接解引用是合理的，因为没有地址空间隔离

---

## 4. 书面练习题

### 题目 1：多道程序调度

**问题：** ch3 使用时间片轮转调度。假设有 3 个任务 T0、T1、T2，时间片为 12500 个时钟周期。T1 在第 2 个时间片内调用了 `sched_yield` 主动让出 CPU。请描述从 T1 调用 `yield` 到 T2 开始执行的完整流程（包括 trap 处理、调度决策、上下文切换）。

**参考答案：**
1. T1 执行 `ecall`，触发 `UserEnvCall` 异常，CPU 切换到 S-mode
2. `scause` 寄存器记录异常原因为 `UserEnvCall`
3. `tcb.handle_syscall()` 识别 syscall ID 为 `SCHED_YIELD`，返回 `SchedulingEvent::Yield`
4. 主循环收到 `Yield` 事件，`break` 退出内层循环，不标记 T1 为完成
5. 外层循环 `i = (i + 1) % index_mod`，切换到 T2
6. 为 T2 设置新的时钟中断（`tg_sbi::set_timer(time::read64() + 12500)`）
7. `tcb.execute()` 恢复 T2 的 `LocalContext`，执行 `sret` 进入 U-mode

---

### 题目 2：syscall 计数实现

**问题：** 以下代码片段有一个 bug，导致 `trace(2, 410, 0)` 查询 `sys_trace` 自身的调用次数时，返回值比实际少 1。请找出 bug 并修复：

```rust
pub fn handle_syscall(&mut self) -> SchedulingEvent {
    let id = self.ctx.a(7).into();
    let args = [self.ctx.a(0), self.ctx.a(1), self.ctx.a(2),
                self.ctx.a(3), self.ctx.a(4), self.ctx.a(5)];

    match tg_syscall::handle(Caller { entity: self as *mut _ as usize, flow: 0 }, id, args) {
        Ret::Done(ret) => {
            let id_num: usize = self.ctx.a(7);
            if id_num < 512 { self.syscall_counts[id_num] += 1; }  // 计数在 handle 之后
            // ...
        }
    }
}
```

**参考答案：**

Bug：计数更新在 `tg_syscall::handle()` 之后。当 `trace(2, 410, 0)` 被调用时，`handle()` 内部执行 `Trace::trace()`，此时 `syscall_counts[410]` 还未更新，所以返回的是上一次的计数（少 1）。

修复：将计数更新移到 `handle()` 调用之前：

```rust
let id_num: usize = self.ctx.a(7);
if id_num < 512 { self.syscall_counts[id_num] += 1; }  // 先更新

match tg_syscall::handle(...) { ... }
```

---

### 题目 3：trace 语义分析

**问题：** 用户程序执行以下代码序列：

```c
int x = 42;
trace(1, &x, 100);   // 写入 100 到 x 的地址
int val = trace(0, &x, 0);  // 读取 x 的地址处的值
```

请回答：
1. `val` 的值是多少？
2. 如果 `&x` 是一个未初始化的栈地址（超出用户栈范围），在 ch3 中会发生什么？在 ch4 中会发生什么？

**参考答案：**

1. `val = 100`。`trace(1, &x, 100)` 将 100 写入 `x` 的地址，`trace(0, &x, 0)` 读取同一地址，返回 100。

2. **ch3**：直接解引用无效地址，可能触发硬件异常（如 load/store access fault），内核捕获后杀死该任务（`log::error!("app{i} was killed by {e:?}")`），不会影响其他任务。

   **ch4**：`address_space.translate(VAddr::new(addr), READABLE)` 返回 `None`（地址未映射），`trace` 返回 -1，不会触发硬件异常，内核继续正常运行。这体现了地址空间隔离的安全性优势。
