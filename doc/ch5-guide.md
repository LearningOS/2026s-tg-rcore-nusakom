# ch5 实验指导：进程管理、spawn 与 stride 调度

## 1. 背景与原理

### 1.1 进程抽象

ch5 引入完整的**进程**概念。进程是操作系统管理资源的基本单位，包含：

```rust
pub struct Process {
    pub pid: ProcId,                              // 唯一进程标识符
    pub context: ForeignContext,                  // 执行上下文（含 satp）
    pub address_space: AddressSpace<Sv39, Sv39Manager>, // 独立地址空间
    pub heap_bottom: usize,                       // 堆底地址
    pub program_brk: usize,                       // 当前堆顶（sbrk 调整）
    pub stride: usize,                            // stride 调度：累计步长
    pub priority: usize,                          // stride 调度：优先级
}
```

与 ch4 的 `Process` 相比，ch5 新增了：
- `pid`：进程 ID，由 `ProcId::new()` 自动分配
- `fork()`/`exec()` 方法：支持进程创建和程序替换
- `stride`/`priority`：支持带优先级的调度

### 1.2 spawn vs fork+exec

传统 UNIX 进程创建使用 `fork + exec` 两步：
1. `fork`：复制父进程的完整地址空间（深拷贝所有物理页）
2. `exec`：用新程序替换当前地址空间

**问题**：`fork` 的地址空间复制开销很大，但 `exec` 会立即丢弃这份拷贝。

`spawn` 直接从 ELF 创建新进程，跳过地址空间复制：

```
fork + exec：父进程地址空间 → 深拷贝 → 丢弃 → 加载新 ELF
spawn：                                          直接加载新 ELF
```

ch5 的 `spawn` 实现：

```rust
fn spawn(&self, _caller: Caller, path: usize, count: usize) -> isize {
    // 1. 从用户空间读取程序名
    let app_data = current.address_space
        .translate::<u8>(VAddr::new(path), READABLE)
        .map(|ptr| unsafe { str::from_utf8_unchecked(slice::from_raw_parts(ptr.as_ptr(), count)) })
        .and_then(|name| APPS.get(name).copied());  // 从内存表查找 ELF

    // 2. 直接从 ELF 创建子进程（不复制父进程地址空间）
    match ProcStruct::from_elf(ElfFile::new(data).unwrap()) {
        Some(child_proc) => {
            let pid = child_proc.pid;
            unsafe { (*processor).add(pid, child_proc, parent_pid) };
            pid.get_usize() as isize  // 返回子进程 PID
        }
        None => -1,
    }
}
```

### 1.3 stride 调度算法

**目标**：为每个进程分配与其优先级成正比的 CPU 时间。

**核心思想**：
- 每个进程有一个 `stride`（累计步长），初始为 0
- 每次调度选择 `stride` 最小的进程运行
- 运行后将该进程的 `stride` 增加 `pass = BIG_STRIDE / priority`

**数学直觉**：优先级高（priority 大）的进程，pass 小，stride 增长慢，因此被更频繁地选中。

```
BIG_STRIDE = 1 << 20 = 1048576

进程 A：priority=4, pass=262144
进程 B：priority=2, pass=524288

调度序列（stride 最小优先）：
  初始：A.stride=0, B.stride=0 → 选 A（或 B，相等时任选）
  A 运行后：A.stride=262144, B.stride=0 → 选 B
  B 运行后：A.stride=262144, B.stride=524288 → 选 A
  A 运行后：A.stride=524288, B.stride=524288 → 选 A（或 B）
  ...
  结果：A 运行 2 次，B 运行 1 次，比例 = 4:2 = 2:1 ✓
```

**溢出处理**：使用 `wrapping_add` 避免 `usize` 溢出导致的错误比较：

```rust
proc.stride = proc.stride.wrapping_add(BIG_STRIDE / proc.priority);
```

---

## 2. 实现思路

### 2.1 扩展 Process 结构体

```rust
pub const BIG_STRIDE: usize = 1 << 20;

pub struct Process {
    // ... 其他字段
    pub stride: usize,    // 初始值：0
    pub priority: usize,  // 初始值：16，约束：>= 2
}
```

在 `from_elf()` 和 `fork()` 中初始化：

```rust
Some(Self {
    // ...
    stride: 0,
    priority: 16,
})
```

### 2.2 实现 ProcManager::fetch()（stride 调度）

将 `VecDeque` 替换为 `Vec`，实现线性扫描找最小 stride：

```rust
pub struct ProcManager {
    tasks: BTreeMap<ProcId, Process>,
    ready_queue: Vec<ProcId>,  // 改为 Vec，支持随机删除
}

impl Schedule<ProcId> for ProcManager {
    fn fetch(&mut self) -> Option<ProcId> {
        if self.ready_queue.is_empty() { return None; }

        // 线性扫描找 stride 最小的进程
        let idx = self.ready_queue.iter().enumerate()
            .min_by_key(|(_, pid)| {
                self.tasks.get(pid).map(|p| p.stride).unwrap_or(0)
            })
            .map(|(i, _)| i)?;

        let pid = self.ready_queue.remove(idx);

        // 更新 stride
        if let Some(proc) = self.tasks.get_mut(&pid) {
            let pass = BIG_STRIDE / proc.priority;
            proc.stride = proc.stride.wrapping_add(pass);
        }
        Some(pid)
    }
}
```

### 2.3 实现 set_priority

```rust
fn set_priority(&self, _caller: Caller, prio: isize) -> isize {
    if prio < 2 { return -1; }  // 优先级必须 >= 2
    let current = PROCESSOR.get_mut().current().unwrap();
    current.priority = prio as usize;
    prio  // 成功返回 prio 本身
}
```

---

## 3. AI 协作过程记录

### 3.1 提示词示例

**问题 1：stride 溢出**
```
stride 调度中，如果 stride 使用 usize 存储，经过大量调度后会溢出。
wrapping_add 能正确处理溢出吗？两个 wrapping 后的 stride 值比较是否仍然正确？
```

**问题 2：spawn 与 fork 的区别**
```
在 ch5 的 tg-rcore-tutorial 框架中，spawn 和 fork+exec 的主要区别是什么？
spawn 的实现中为什么不需要调用 cloneself()？
```

### 3.2 关键洞察

- stride 调度的公平性依赖于 `BIG_STRIDE` 足够大，使得整数除法误差可忽略
- `wrapping_add` 在溢出时仍能保持相对顺序（因为所有进程的 stride 都在同一"环"上）
- `spawn` 的关键是直接调用 `from_elf()`，而不是先 `fork()` 再 `exec()`

---

## 4. 书面练习题

### 题目 1：stride 调度公平性

**问题：** 有 3 个进程 P1、P2、P3，优先级分别为 2、4、8，`BIG_STRIDE = 1024`。初始 stride 均为 0。请列出前 7 次调度的选择顺序，并验证最终各进程被调度的次数比例是否符合优先级比例（1:2:4）。

**参考答案：**

pass 值：P1=512, P2=256, P3=128

| 调度次 | 选择 | P1.stride | P2.stride | P3.stride |
|--------|------|-----------|-----------|-----------|
| 初始   | -    | 0         | 0         | 0         |
| 1      | P1（或任意，相等） | 512 | 0 | 0 |
| 2      | P2   | 512       | 256       | 0         |
| 3      | P3   | 512       | 256       | 128       |
| 4      | P3   | 512       | 256       | 256       |
| 5      | P2   | 512       | 512       | 256       |
| 6      | P3   | 512       | 512       | 384       |
| 7      | P3   | 512       | 512       | 512       |

前 7 次：P1=1次, P2=2次, P3=4次，比例 = 1:2:4 ✓

---

### 题目 2：spawn 实现分析

**问题：** 以下是一个错误的 `spawn` 实现，它先 fork 再 exec。请指出这种实现的问题，并说明为什么 ch5 的正确实现不需要 fork：

```rust
fn spawn_wrong(&self, _caller: Caller, path: usize, count: usize) -> isize {
    // 错误：先 fork 复制父进程地址空间
    let child_pid = self.fork(_caller);
    if child_pid == 0 {
        // 子进程：exec 替换地址空间
        self.exec(_caller, path, count);
    }
    child_pid
}
```

**参考答案：**

问题：
1. **性能浪费**：`fork` 需要深拷贝父进程的整个地址空间（包括所有物理页），但 `exec` 会立即丢弃这份拷贝，造成大量无效内存分配和复制
2. **语义不同**：`fork` 创建的子进程初始状态与父进程相同（包括 PC、寄存器），而 `spawn` 应该直接从目标程序的入口点开始执行
3. **父子进程判断**：上述代码在内核中无法用 `child_pid == 0` 判断当前是父进程还是子进程（这是用户态的判断逻辑）

正确实现不需要 fork 的原因：`spawn` 直接调用 `Process::from_elf()` 从 ELF 文件创建全新的进程，分配新的地址空间、新的 PID 和新的执行上下文，完全独立于父进程。

---

### 题目 3：优先级约束

**问题：** 为什么 stride 调度要求优先级 `priority >= 2`？如果允许 `priority = 1`，会有什么问题？如果允许 `priority = 0`，会发生什么？

**参考答案：**

- **priority = 1**：`pass = BIG_STRIDE / 1 = BIG_STRIDE`，该进程每次调度后 stride 增加最大值，会被调度最少次。虽然技术上可行，但与"优先级越高调度越频繁"的语义相反（priority=1 应该是最低优先级），容易造成混淆。框架要求 `priority >= 2` 是为了保证语义清晰。

- **priority = 0**：`pass = BIG_STRIDE / 0`，整数除以零会导致 panic（Rust 的 `usize` 除法在 debug 模式下 panic，release 模式下未定义行为）。这是一个严重的安全问题，因此必须在 `set_priority` 中拒绝 `prio <= 1` 的值。
