# ch4 实验指导：地址空间、trace 重写与 mmap/munmap

## 1. 背景与原理

### 1.1 Sv39 虚拟内存

ch4 引入 **RISC-V Sv39** 三级页表，为每个进程提供独立的虚拟地址空间。

Sv39 地址结构（39 位虚拟地址）：

```
虚拟地址（39 位）：
  [38:30] VPN[2]  9 位  → 一级页表索引
  [29:21] VPN[1]  9 位  → 二级页表索引
  [20:12] VPN[0]  9 位  → 三级页表索引
  [11:0]  offset  12 位 → 页内偏移（4 KiB 页）
```

地址翻译流程：

```
satp.ppn → 根页表 → PTE[VPN[2]] → 二级页表 → PTE[VPN[1]] → 三级页表 → PTE[VPN[0]] → 物理页 + offset
```

### 1.2 异界传送门（MultislotPortal）

**问题**：切换 `satp`（页表基地址）时，当前执行的代码地址会立即失效。

**解决方案**：异界传送门——在内核和所有用户地址空间的**相同虚拟地址**（`VPN::MAX`，即地址空间最高页）映射**同一物理页**。

```
内核地址空间：  VPN::MAX → 传送门物理页
用户地址空间：  VPN::MAX → 传送门物理页（相同）
```

切换 `satp` 时，传送门代码的虚拟地址不变，因此可以继续执行。

### 1.3 页表标志位格式

框架使用 5 字节字符串表示页表项权限：

```
格式：[U][X][W][R][V]
  位置 0: 'U' 或 '_' = 用户态可访问
  位置 1: 'X' 或 '_' = 可执行
  位置 2: 'W' 或 '_' = 可写
  位置 3: 'R' 或 '_' = 可读
  位置 4: 'V'        = 有效位（必须为 V）

示例：
  "U_WRV" = 用户态可读写
  "X_RV"  = 内核代码段（可执行可读）
  "RV"    = 只读（用于 translate 权限检查）
  "W_V"   = 只写（用于 translate 权限检查）
```

### 1.4 地址翻译 API

```rust
// 将用户虚拟地址翻译为内核可访问的物理指针，同时检查权限
address_space.translate::<T>(VAddr::new(addr), flags) -> Option<NonNull<T>>
// 返回 None 表示地址未映射或权限不符
```

---

## 2. 实现思路

### 2.1 重写 sys_trace（带权限检查）

ch4 中用户地址需要通过页表翻译，不能直接解引用：

```rust
impl Trace for SyscallContext {
    fn trace(&self, caller: Caller, trace_request: usize, id: usize, data: usize) -> isize {
        let process = unsafe { PROCESSES.get_mut() }
            .get_mut(caller.entity).unwrap();
        match trace_request {
            0 => {
                // 读操作：需要 R+V 权限
                const READABLE: VmFlags<Sv39> = build_flags("RV");
                if let Some(ptr) = process.address_space
                    .translate::<u8>(VAddr::new(id), READABLE)
                {
                    unsafe { *ptr.as_ptr() as isize }
                } else {
                    -1  // 地址不可读，返回 -1
                }
            }
            1 => {
                // 写操作：需要 W+V 权限
                const WRITABLE: VmFlags<Sv39> = build_flags("W_V");
                if let Some(mut ptr) = process.address_space
                    .translate::<u8>(VAddr::new(id), WRITABLE)
                {
                    unsafe { *ptr.as_mut() = data as u8 };
                    0
                } else {
                    -1  // 地址不可写，返回 -1
                }
            }
            _ => -1,
        }
    }
}
```

注意：ch4 的 `trace` 不再支持 `trace_request=2`（syscall 计数），因为 ch4 没有 `syscall_counts` 字段。

### 2.2 实现 mmap

参数验证顺序（严格按此顺序，避免遗漏）：

```rust
fn mmap(&self, caller: Caller, addr: usize, len: usize, prot: i32, ...) -> isize {
    // 1. addr 必须页对齐
    if addr & ((1 << Sv39::PAGE_BITS) - 1) != 0 { return -1; }

    // 2. prot 高位必须为 0（只有低 3 位有效）
    if prot & !0b111 != 0 { return -1; }

    // 3. len 为 0 直接返回成功（无需映射）
    if len == 0 { return 0; }

    // 4. 构建权限标志（注意 prot 位顺序与标志位格式的对应关系）
    //    prot: bit0=R, bit1=W, bit2=X（Linux 约定）
    //    flags: [U][X][W][R][V]
    let mut flags_str = [b'U', b'_', b'_', b'_', b'V'];
    if prot & 0b100 != 0 { flags_str[3] = b'R'; }  // bit2 → R
    if prot & 0b010 != 0 { flags_str[2] = b'W'; }  // bit1 → W
    if prot & 0b001 != 0 { flags_str[1] = b'X'; }  // bit0 → X（注意：Linux bit0=R，这里是X）

    // 5. 映射虚拟地址范围
    let start = VAddr::<Sv39>::new(addr).floor();
    let end = VAddr::<Sv39>::new(addr + len).ceil();
    process.address_space.map(start..end, &[], 0, flags);
    0
}
```

**prot 位顺序说明**：Linux `mmap` 的 `prot` 参数：
- `PROT_READ = 1`（bit0）
- `PROT_WRITE = 2`（bit1）
- `PROT_EXEC = 4`（bit2）

框架标志位格式中 R/W/X 的位置与 prot 不同，需要正确映射。

### 2.3 实现 munmap

```rust
fn munmap(&self, caller: Caller, addr: usize, len: usize) -> isize {
    if addr & ((1 << Sv39::PAGE_BITS) - 1) != 0 { return -1; }
    if len == 0 { return 0; }
    let start = VAddr::<Sv39>::new(addr).floor();
    let end = VAddr::<Sv39>::new(addr + len).ceil();
    process.address_space.unmap(start..end);
    0
}
```

---

## 3. AI 协作过程记录

### 3.1 提示词示例

**问题 1：理解异界传送门**
```
在 ch4 中，为什么需要异界传送门（MultislotPortal）？
切换 satp 时会发生什么问题？传送门是如何解决这个问题的？
```

**问题 2：prot 位顺序**
```
Linux mmap 的 prot 参数中，PROT_READ=1, PROT_WRITE=2, PROT_EXEC=4。
但 tg-rcore-tutorial 的页表标志位格式是 "U[X][W][R]V"，
请帮我写出 prot 到标志位字符串的正确映射代码。
```

### 3.2 关键洞察

- `translate()` 返回 `Option<NonNull<T>>`，`None` 表示地址无效或权限不符，这是 ch4 安全性的核心
- `mmap` 的 `prot` 参数位顺序与框架标志位格式不同，容易出错
- `munmap` 不需要检查地址是否已映射（框架的 `unmap` 会处理），但测例要求未映射时返回 -1

---

## 4. 书面练习题

### 题目 1：Sv39 地址翻译

**问题：** 给定虚拟地址 `0x10000`（65536），请计算：
1. 该地址的 VPN[2]、VPN[1]、VPN[0] 和页内偏移各是多少？
2. 如果根页表的物理地址为 `0x80200000`，且 `PTE[VPN[2]]` 指向物理地址 `0x80201000` 的二级页表，`PTE[VPN[1]]` 指向 `0x80202000` 的三级页表，`PTE[VPN[0]]` 的 PPN 为 `0x80203`，则虚拟地址 `0x10000` 对应的物理地址是多少？

**参考答案：**

1. `0x10000 = 0b 000_000_000 | 000_000_000 | 000_010_000 | 0000_0000_0000`
   - VPN[2] = 0
   - VPN[1] = 0
   - VPN[0] = `0x10000 >> 12 & 0x1FF` = `0x10 & 0x1FF` = 16
   - 页内偏移 = `0x10000 & 0xFFF` = 0

2. 物理地址 = PPN × 4096 + offset = `0x80203 × 0x1000 + 0` = `0x80203000`

---

### 题目 2：mmap 权限标志

**问题：** 用户程序调用 `mmap(0x10000, 4096, PROT_READ | PROT_WRITE, ...)` 映射一页内存。请写出对应的页表标志位字符串，并解释为什么必须包含 `U` 标志。

**参考答案：**

`PROT_READ | PROT_WRITE = 1 | 2 = 3 = 0b011`

对应标志位：
- bit0 (PROT_READ=1) → R → `flags_str[3] = 'R'`
- bit1 (PROT_WRITE=2) → W → `flags_str[2] = 'W'`

最终标志位字符串：`"U_WRV"`

必须包含 `U` 标志的原因：RISC-V 页表项的 U 位（User bit）控制用户态（U-mode）是否可以访问该页。如果不设置 U 位，用户程序访问该页时会触发 Page Fault，即使 R/W 位已设置。内核态（S-mode）可以访问所有页（无论 U 位），但用户态只能访问 U 位为 1 的页。

---

### 题目 3：重叠映射检测

**问题：** 以下代码序列的执行结果是什么？请解释原因。

```c
int ret1 = mmap(0x10000, 8192, PROT_READ | PROT_WRITE, ...);  // 映射 2 页
int ret2 = mmap(0x11000, 4096, PROT_READ, ...);               // 尝试映射第 2 页
int ret3 = mmap(0x12000, 4096, PROT_WRITE, ...);              // 映射第 3 页
```

**参考答案：**

- `ret1 = 0`：成功映射 `[0x10000, 0x12000)` 两页
- `ret2 = -1`：`0x11000` 在已映射范围 `[0x10000, 0x12000)` 内，重叠映射，返回 -1
- `ret3 = 0`：`0x12000` 未被映射，成功映射

注意：框架的 `address_space.map()` 在遇到已映射页时的行为取决于实现。测例要求检测重叠并返回 -1，但当前框架实现可能不会自动检测——这是一个需要在 `mmap` 实现中手动检查的边界条件。
