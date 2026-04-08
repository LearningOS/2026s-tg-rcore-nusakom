# ch6 实验指导：文件系统与硬链接

## 1. 背景与原理

### 1.1 EasyFs 文件系统结构

ch6 使用 `tg-rcore-tutorial-easy-fs`（EasyFs），一个简单的类 UNIX inode 文件系统。

磁盘布局：

```
| SuperBlock | Inode Bitmap | Inode Area | Data Bitmap | Data Area |
```

- **SuperBlock**：记录各区域的块数和起始位置
- **Inode Bitmap**：位图，标记哪些 inode 已被分配
- **Inode Area**：存储所有 `DiskInode` 结构体
- **Data Bitmap**：位图，标记哪些数据块已被分配
- **Data Area**：存储文件实际数据

### 1.2 DiskInode 结构

`DiskInode` 是磁盘上的 inode 元数据：

```rust
pub struct DiskInode {
    pub size: u32,                          // 文件大小（字节）
    pub direct: [u32; 28],                  // 直接块指针（28 个）
    pub indirect1: u32,                     // 一级间接块指针
    pub indirect2: u32,                     // 二级间接块指针
    type_: DiskInodeType,                   // 文件类型（File/Directory）
    pub nlink: u32,                         // 硬链接计数（ch6 新增）
}
```

`nlink` 字段是 ch6 新增的，记录指向该 inode 的目录项数量。

### 1.3 硬链接概念

**硬链接**：多个目录项（directory entry）指向同一个 inode。

```
目录：
  "file.txt"  → inode #5
  "link.txt"  → inode #5  ← 硬链接，指向同一 inode

inode #5：
  nlink = 2   ← 有 2 个目录项指向它
  data blocks → 实际文件内容
```

关键特性：
- 硬链接与原文件完全等价，没有"原文件"和"链接文件"之分
- 删除任意一个目录项，只要 `nlink > 0`，文件内容不会被删除
- 只有当 `nlink == 0` 时，才回收 inode 和数据块

**与软链接（符号链接）的区别**：

| 特性 | 硬链接 | 软链接 |
|------|--------|--------|
| 指向 | inode（磁盘位置） | 路径字符串 |
| 跨文件系统 | 不支持 | 支持 |
| 原文件删除后 | 仍可访问 | 链接失效 |
| 占用 inode | 不额外占用 | 占用新 inode |

### 1.4 三层架构

硬链接实现涉及三层：

```
用户态 syscall (linkat/unlinkat/fstat)
    ↓
内核 FileSystem (ch6/src/fs.rs) - FSManager trait
    ↓
VFS Inode (easy-fs/src/vfs.rs) - link/unlink/inode_id/nlink
    ↓
DiskInode (easy-fs/src/layout.rs) - nlink 字段
```

---

## 2. 实现思路

### 2.1 修改 DiskInode（添加 nlink 字段）

在 `easy-fs/src/layout.rs` 中：

```rust
pub struct DiskInode {
    pub size: u32,
    pub direct: [u32; INODE_DIRECT_COUNT],
    pub indirect1: u32,
    pub indirect2: u32,
    type_: DiskInodeType,
    pub nlink: u32,  // 新增：硬链接计数
}

impl DiskInode {
    pub fn initialize(&mut self, type_: DiskInodeType) {
        self.size = 0;
        // ...
        self.nlink = 1;  // 初始值为 1（创建时就有一个目录项指向它）
    }
}
```

### 2.2 在 Inode 中实现 link/unlink

**link 流程**：

```rust
pub fn link(&self, src: &str, dst: &str) -> isize {
    // 1. 查找 src 的 inode_id
    let inode_id = self.read_disk_inode(|disk_inode| {
        self.find_inode_id(src, disk_inode)
    });
    let inode_id = match inode_id { Some(id) => id, None => return -1 };

    // 2. 在目录中追加新的目录项（dst → inode_id）
    let mut fs = self.fs.lock();
    self.modify_disk_inode(|root_inode| {
        let file_count = (root_inode.size as usize) / DIRENT_SZ;
        let new_size = (file_count + 1) * DIRENT_SZ;
        self.increase_size(new_size as u32, root_inode, &mut fs);
        let dirent = DirEntry::new(dst, inode_id);
        root_inode.write_at(file_count * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
    });

    // 3. 目标 inode 的 nlink += 1
    let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
    get_block_cache(block_id as usize, Arc::clone(&self.block_device))
        .lock()
        .modify(block_offset, |disk_inode: &mut DiskInode| {
            disk_inode.nlink += 1;
        });
    block_cache_sync_all();
    0
}
```

**unlink 流程**：

```rust
pub fn unlink(&self, name: &str) -> isize {
    // 1. 找到目录项（记录索引和 inode_id）
    let result = self.read_disk_inode(|disk_inode| {
        // 遍历目录项，找到 name 对应的条目
        // 返回 (entry_idx, inode_id, total_file_count)
    });
    let (entry_idx, inode_id, file_count) = match result { Some(v) => v, None => return -1 };

    // 2. 删除目录项（用最后一项覆盖，然后缩小目录大小）
    self.modify_disk_inode(|root_inode| {
        if entry_idx < file_count - 1 {
            // 将最后一个目录项移到被删除的位置
            let mut last_dirent = DirEntry::empty();
            root_inode.read_at(DIRENT_SZ * (file_count - 1), last_dirent.as_bytes_mut(), ...);
            root_inode.write_at(DIRENT_SZ * entry_idx, last_dirent.as_bytes(), ...);
        }
        root_inode.size -= DIRENT_SZ as u32;
    });

    // 3. 目标 inode 的 nlink -= 1
    let new_nlink = /* 修改 disk_inode.nlink -= 1，返回新值 */;

    // 4. 如果 nlink == 0，回收 inode 和数据块
    if new_nlink == 0 {
        // 清空数据块，释放 inode bitmap 位
        fs.inode_bitmap.dealloc(&self.block_device, inode_id as usize);
    }
    0
}
```

### 2.3 实现 inode_id() 方法

`fstat` 需要返回 inode 编号，通过 block_id 和 block_offset 反推：

```rust
pub fn inode_id(&self) -> u32 {
    let fs = self.fs.lock();
    let inode_size = core::mem::size_of::<DiskInode>();
    let inodes_per_block = (BLOCK_SZ / inode_size) as u32;
    let block_offset_in_inode_area = self.block_id as u32 - fs.inode_area_start_block;
    block_offset_in_inode_area * inodes_per_block + (self.block_offset / inode_size) as u32
}
```

### 2.4 实现 fstat 系统调用

```rust
fn fstat(&self, _caller: Caller, fd: usize, st: usize) -> isize {
    let current = PROCESSOR.get_mut().current().unwrap();
    // 从 fd_table 获取 inode
    let inode = match &current.fd_table[fd] {
        Some(file) => file.lock().inode.clone()?,
        None => return -1,
    };
    // 翻译用户地址，写入 Stat 结构体
    if let Some(mut ptr) = current.address_space
        .translate::<Stat>(VAddr::new(st), WRITEABLE)
    {
        let mut stat = Stat::new();
        stat.dev = 0;
        stat.ino = inode.inode_id() as u64;
        stat.mode = StatMode::FILE;
        stat.nlink = inode.nlink();
        unsafe { *ptr.as_mut() = stat; }
        0
    } else { -1 }
}
```

---

## 3. AI 协作过程记录

### 3.1 提示词示例

**问题 1：inode_id 计算**
```
在 easy-fs 中，Inode 结构体存储了 block_id 和 block_offset。
如何从这两个值反推出 inode_id（即该 inode 在 inode 区域中的编号）？
需要用到 EasyFileSystem 的哪些字段？
```

**问题 2：unlink 删除目录项**
```
在 easy-fs 的目录中删除一个目录项时，如果直接将该项清零，
会导致目录项数组出现"空洞"，遍历时需要跳过空项。
有没有更简单的方法？
```

### 3.2 关键洞察

- 删除目录项用"最后一项覆盖"的方式，避免空洞，简化遍历逻辑
- `inode_area_start_block` 需要设为 `pub` 才能在 `vfs.rs` 中访问
- `nlink` 初始值为 1（不是 0），因为创建文件时就有一个目录项指向它

---

## 4. 书面练习题

### 题目 1：硬链接 vs 软链接

**问题：** 执行以下操作序列后，回答问题：

```bash
echo "hello" > file.txt          # 创建文件
linkat file.txt link1.txt        # 创建硬链接
linkat file.txt link2.txt        # 再创建一个硬链接
unlinkat file.txt                # 删除原始目录项
```

1. 执行完所有操作后，`link1.txt` 和 `link2.txt` 是否仍然可以读取？
2. 此时 `fstat(link1.txt).nlink` 的值是多少？
3. 如果改用软链接（符号链接），`unlinkat file.txt` 后 `link1.txt` 是否仍可读取？

**参考答案：**

1. **可以读取**。`unlinkat file.txt` 只删除了 `file.txt` 这个目录项，inode 的 `nlink` 从 3 变为 2，文件内容（数据块）不会被删除。`link1.txt` 和 `link2.txt` 仍然指向同一个 inode，可以正常读取。

2. `nlink = 2`（`link1.txt` 和 `link2.txt` 各贡献 1）。

3. **不可读取**。软链接存储的是路径字符串 `"file.txt"`，当 `file.txt` 被删除后，软链接指向的路径不存在，访问时会报"文件不存在"错误（dangling symlink）。

---

### 题目 2：nlink 计数语义

**问题：** 以下代码序列执行后，各步骤的 `nlink` 值是多少？

```
1. 创建文件 a.txt
2. linkat a.txt b.txt
3. linkat a.txt c.txt
4. unlinkat b.txt
5. unlinkat a.txt
6. unlinkat c.txt
```

**参考答案：**

| 步骤 | 操作 | nlink |
|------|------|-------|
| 1 | 创建 a.txt | 1 |
| 2 | linkat a.txt b.txt | 2 |
| 3 | linkat a.txt c.txt | 3 |
| 4 | unlinkat b.txt | 2 |
| 5 | unlinkat a.txt | 1 |
| 6 | unlinkat c.txt | 0 → 回收 inode 和数据块 |

步骤 6 后，inode 被回收，文件内容永久删除。

---

### 题目 3：inode 回收条件

**问题：** 在 `unlink` 实现中，当 `nlink` 降为 0 时需要回收 inode 和数据块。请描述回收的完整步骤，并说明为什么需要同时回收 inode bitmap 位和数据块。

**参考答案：**

回收步骤：
1. **清空数据块**：调用 `disk_inode.clear_size()`，返回所有数据块的块号列表
2. **释放数据块**：对每个数据块调用 `fs.dealloc_data(block_id)`，清零内容并在 data bitmap 中标记为空闲
3. **释放 inode**：调用 `fs.inode_bitmap.dealloc(&block_device, inode_id)`，在 inode bitmap 中标记该 inode 为空闲
4. **同步缓存**：调用 `block_cache_sync_all()` 将修改写回磁盘

为什么需要同时回收两者：
- **数据块**：存储文件实际内容，不回收会导致磁盘空间泄漏
- **inode bitmap 位**：标记 inode 槽位是否可用，不回收会导致 inode 耗尽（即使数据块已释放，新文件也无法创建）

两者必须同时回收，否则文件系统会出现不一致状态。
