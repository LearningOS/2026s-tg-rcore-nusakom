# ch8 实验指导：并发、同步与死锁检测

## 1. 背景与原理

### 1.1 线程与进程

ch8 将"进程"拆分为两个独立抽象：

| 概念 | 管理内容 | 数据结构 |
|------|----------|----------|
| **Process** | 共享资源（地址空间、fd_table、同步原语、信号） | `Process` |
| **Thread** | 执行状态（TID、寄存器上下文） | `Thread` |

同一进程的多个线程**共享地址空间**，但各自有独立的用户栈和执行上下文。

```rust
pub struct Thread {
    pub tid: ThreadId,          // 线程 ID
    pub context: ForeignContext, // 执行上下文（含 satp）
}

pub struct Process {
    pub pid: ProcId,
    pub address_space: AddressSpace<Sv39, Sv39Manager>,
    pub fd_table: Vec<Option<Mutex<Fd>>>,
    pub semaphore_list: Vec<Option<Arc<Semaphore>>>,
    pub mutex_list: Vec<Option<Arc<dyn MutexTrait>>>,
    pub condvar_list: Vec<Option<Arc<Condvar>>>,
    // 死锁检测字段（ch8 新增）
    pub deadlock_detect_enabled: bool,
    pub mutex_available: Vec<usize>,
    pub mutex_allocation: Vec<Vec<usize>>,
    pub mutex_need: Vec<Vec<usize>>,
    pub sem_available: Vec<usize>,
    pub sem_allocation: Vec<Vec<usize>>,
    pub sem_need: Vec<Vec<usize>>,
}
```

### 1.2 同步原语

**Mutex（互斥锁）**：保证同一时刻只有一个线程进入临界区。

```
线程 A: lock() → 临界区 → unlock()
线程 B: lock() → 阻塞等待 → (A unlock 后唤醒) → 临界区 → unlock()
```

**Semaphore（信号量）**：计数型资源管理，支持多个线程同时访问有限资源。

```
初始计数 = N（可用资源数）
P 操作（down）：计数 -= 1，若计数 < 0 则阻塞
V 操作（up）：计数 += 1，若有等待线程则唤醒一个
```

### 1.3 死锁的四个必要条件

死锁发生需要同时满足：
1. **互斥**：资源一次只能被一个线程使用
2. **持有并等待**：线程持有资源的同时等待其他资源
3. **不可抢占**：资源只能由持有者主动释放
4. **循环等待**：线程间形成等待环路

### 1.4 银行家算法

银行家算法通过**安全性检测**预防死锁：在分配资源前，检查分配后系统是否仍处于"安全状态"。

**三个数据结构**：

```
Available[j]       = 资源 j 当前可用数量
Allocation[i][j]   = 线程 i 已持有资源 j 的数量
Need[i][j]         = 线程 i 还需要资源 j 的数量
```

**安全性算法**：

```
Work = Available.clone()
Finish = [false; n]

loop:
  找到满足以下条件的线程 i：
    Finish[i] == false
    Need[i][j] <= Work[j]  对所有 j 成立
  
  若找到：
    Work[j] += Allocation[i][j]  // 模拟线程 i 完成并释放资源
    Finish[i] = true
  
  若找不到：退出循环

若 Finish.all(true)：安全状态（可以分配）
否则：不安全状态（拒绝分配，返回 -0xDEAD）
```

---

## 2. 实现思路

### 2.1 Process 中的银行家算法矩阵

mutex 和 semaphore 分别维护独立的矩阵（不混合检测）：

```rust
// mutex：每个 mutex 视为 1 个资源
pub mutex_available: Vec<usize>,        // Available[mutex_id]
pub mutex_allocation: Vec<Vec<usize>>,  // Allocation[tid_idx][mutex_id]
pub mutex_need: Vec<Vec<usize>>,        // Need[tid_idx][mutex_id]

// semaphore：初始值为信号量的 res_count
pub sem_available: Vec<usize>,          // Available[sem_id]
pub sem_allocation: Vec<Vec<usize>>,    // Allocation[tid_idx][sem_id]
pub sem_need: Vec<Vec<usize>>,          // Need[tid_idx][sem_id]
```

`tid_idx` 是线程在进程线程列表中的索引（不是 TID 本身）。

### 2.2 banker_check() 实现

```rust
pub fn banker_check(
    available: &[usize],
    allocation: &[Vec<usize>],
    need: &[Vec<usize>],
) -> bool {
    let n = allocation.len();  // 线程数
    let m = available.len();   // 资源类型数
    if n == 0 || m == 0 { return true; }

    let mut work = available.to_vec();
    let mut finish = vec![false; n];

    loop {
        let mut found = false;
        for i in 0..n {
            if finish[i] { continue; }
            // 检查 Need[i] <= Work
            let can = (0..m).all(|j| {
                need[i].get(j).copied().unwrap_or(0) <= work[j]
            });
            if can {
                // 模拟线程 i 完成，释放资源
                for j in 0..m {
                    work[j] += allocation[i].get(j).copied().unwrap_or(0);
                }
                finish[i] = true;
                found = true;
            }
        }
        if !found { break; }
    }
    finish.iter().all(|&f| f)
}
```

### 2.3 mutex_lock 中的死锁检测

"试探性更新 → 检查 → 回滚"模式：

```rust
fn mutex_lock(&self, _caller: Caller, mutex_id: usize) -> isize {
    // ...
    if current_proc.deadlock_detect_enabled {
        // 1. 更新 Need 矩阵（表示线程请求该资源）
        current_proc.mutex_need[tid_idx][mutex_id] += 1;

        if current_proc.mutex_available[mutex_id] > 0 {
            // 2. 试探性分配
            current_proc.mutex_available[mutex_id] -= 1;
            current_proc.mutex_allocation[tid_idx][mutex_id] += 1;
            current_proc.mutex_need[tid_idx][mutex_id] -= 1;

            // 3. 安全性检查
            let safe = Process::banker_check(
                &current_proc.mutex_available,
                &current_proc.mutex_allocation,
                &current_proc.mutex_need,
            );

            if !safe {
                // 4. 不安全：回滚并拒绝
                current_proc.mutex_available[mutex_id] += 1;
                current_proc.mutex_allocation[tid_idx][mutex_id] -= 1;
                current_proc.mutex_need[tid_idx][mutex_id] += 1;
                return -0xDEAD;
            }
            // 5. 安全：实际加锁
            mutex.lock(tid);
            return 0;
        }
        // 资源不足时也检查安全性...
    }
    // 未启用检测：直接加锁
    if !mutex.lock(tid) { -1 } else { 0 }
}
```

### 2.4 enable_deadlock_detect 实现

```rust
fn enable_deadlock_detect(&self, _caller: Caller, is_enable: i32) -> isize {
    match is_enable {
        0 => { current_proc.deadlock_detect_enabled = false; 0 }
        1 => { current_proc.deadlock_detect_enabled = true; 0 }
        _ => -1,  // 参数不合法
    }
}
```

---

## 3. AI 协作过程记录

### 3.1 提示词示例

**问题 1：tid_idx 的含义**
```
在银行家算法矩阵中，行索引是 tid_idx 而不是 TID 本身。
如何从 TID 获取 tid_idx？在 ch8 的 PThreadManager 中，
get_thread(pid) 返回什么？
```

**问题 2：矩阵动态扩展**
```
银行家算法矩阵的大小在运行时动态变化（新线程创建、新 mutex 创建）。
如何确保矩阵在访问前已经有足够的行和列？
```

### 3.2 关键洞察

- 矩阵行数 = 线程数，列数 = 资源数，两者都是动态变化的，需要 `ensure_*_matrix()` 辅助方法
- mutex 和 semaphore 分开检测，简化了实现（不需要考虑两者混合使用的死锁）
- `-0xDEAD` 不等于 `-1`，主循环中只有 `-1` 才触发线程阻塞，`-0xDEAD` 直接返回给用户态

---

## 4. 书面练习题

### 题目 1：银行家算法手动推导

**问题：** 系统有 3 个线程（T0、T1、T2）和 1 种资源（mutex M0，共 1 个）。当前状态：

```
Available = [0]  (M0 已被占用)
Allocation = [[1], [0], [0]]  (T0 持有 M0)
Need       = [[0], [1], [1]]  (T1、T2 各需要 M0)
```

1. 系统当前是否处于安全状态？请用银行家算法推导。
2. 如果 T1 请求 M0（`mutex_lock`），死锁检测应该返回什么？

**参考答案：**

1. **安全状态分析**：
   - Work = [0], Finish = [false, false, false]
   - 找满足 Need[i] <= Work 的线程：
     - T0: Need=[0] <= Work=[0] ✓ → Work=[0]+[1]=[1], Finish[0]=true
     - T1: Need=[1] <= Work=[1] ✓ → Work=[1]+[0]=[1], Finish[1]=true
     - T2: Need=[1] <= Work=[1] ✓ → Work=[1]+[0]=[1], Finish[2]=true
   - Finish = [true, true, true] → **安全状态**，安全序列为 T0→T1→T2

2. T1 请求 M0 时：
   - 试探性分配：Available=[0]-1=-1（不够！）
   - 资源不足，检查等待后的安全性：
     - Need[1][0] 已为 1（T1 需要 M0），Available=[0]
     - 只有 T0 能完成（Need=[0] <= Work=[0]），T0 完成后 Work=[1]
     - T1 和 T2 都能完成 → 安全
   - 返回 -1（阻塞等待），不返回 -0xDEAD

---

### 题目 2：死锁检测启用时机

**问题：** 以下代码中，死锁检测在哪个时间点启用？如果在 `mutex_create` 之前启用，会有什么问题？

```rust
// 用户程序
let m0 = mutex_create(true);   // 创建 mutex
let m1 = mutex_create(true);   // 创建 mutex
enable_deadlock_detect(1);     // 启用死锁检测
mutex_lock(m0);
mutex_lock(m1);
```

**参考答案：**

死锁检测在 `mutex_create` 之后、`mutex_lock` 之前启用。

如果在 `mutex_create` 之前启用：
- `mutex_create` 会初始化 `mutex_available[id] = 1`
- 但如果死锁检测已启用，`mutex_lock` 时会访问 `mutex_available`，此时矩阵可能还未初始化（大小为 0）
- `ensure_mutex_matrix()` 会自动扩展矩阵，所以实际上不会崩溃，但 `mutex_available` 的初始值会被设为 1（默认值），与 `mutex_create` 设置的值一致，因此结果正确

实际上，当前实现中启用顺序不影响正确性，因为 `ensure_mutex_matrix()` 会在每次访问前确保矩阵大小足够。

---

### 题目 3：-0xDEAD 返回值语义

**问题：** 死锁检测拒绝分配时返回 `-0xDEAD`（十六进制 `0xDEAD = 57005`，即 `-57005`）而不是 `-1`。请解释：
1. 为什么不能返回 `-1`？
2. 主循环如何区分"资源不可用（阻塞）"和"死锁检测拒绝（不阻塞）"？

**参考答案：**

1. 不能返回 `-1` 的原因：`-1` 是 `mutex_lock`/`semaphore_down` 的"资源不可用，需要阻塞"信号。主循环检测到 `ret == -1` 时会调用 `make_current_blocked()`，将线程从就绪队列移除。如果死锁检测也返回 `-1`，线程会被阻塞，但没有任何机制会唤醒它（因为死锁检测拒绝了分配，资源状态没有变化），导致线程永久阻塞。

2. 主循环的区分逻辑：

```rust
Id::SEMAPHORE_DOWN | Id::MUTEX_LOCK | Id::CONDVAR_WAIT => {
    *ctx.a_mut(0) = ret as _;
    if ret == -1 {
        // 资源不可用：阻塞线程，等待资源释放后唤醒
        unsafe { (*processor).make_current_blocked() };
    } else {
        // ret == 0（成功）或 ret == -0xDEAD（死锁拒绝）：
        // 都不阻塞，直接挂起（时间片轮转）
        unsafe { (*processor).make_current_suspend() };
    }
}
```

`-0xDEAD != -1`，所以死锁检测拒绝时走 `else` 分支，线程不会被阻塞，而是正常挂起并在下次调度时继续执行（用户程序可以检查返回值并处理死锁）。
