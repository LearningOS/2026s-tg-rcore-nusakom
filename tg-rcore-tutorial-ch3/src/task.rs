//! 任务管理模块

use tg_kernel_context::LocalContext;
use tg_syscall::{Caller, SyscallId};

/// 系统调用计数表大小（覆盖 0..512 的 syscall ID）
const SYSCALL_COUNT: usize = 512;

/// 任务控制块（Task Control Block, TCB）
pub struct TaskControlBlock {
    /// 用户态上下文
    ctx: LocalContext,
    /// 任务完成标志
    pub finish: bool,
    /// 用户栈：8 KiB
    stack: [usize; 1024],
    /// 系统调用计数表：syscall_counts[id] = 调用次数
    pub syscall_counts: [usize; SYSCALL_COUNT],
}

/// 调度事件
pub enum SchedulingEvent {
    /// 继续执行当前任务
    None,
    /// 任务主动让出 CPU
    Yield,
    /// 任务请求退出，附带退出码
    Exit(usize),
    /// 不支持的系统调用
    UnsupportedSyscall(SyscallId),
}

impl TaskControlBlock {
    /// 零值常量
    pub const ZERO: Self = Self {
        ctx: LocalContext::empty(),
        finish: false,
        stack: [0; 1024],
        syscall_counts: [0; SYSCALL_COUNT],
    };

    /// 初始化任务
    pub fn init(&mut self, entry: usize) {
        self.stack.fill(0);
        self.finish = false;
        self.syscall_counts = [0; SYSCALL_COUNT];
        self.ctx = LocalContext::user(entry);
        *self.ctx.sp_mut() =
            self.stack.as_ptr() as usize + core::mem::size_of_val(&self.stack);
    }

    /// 执行此任务
    #[inline]
    pub unsafe fn execute(&mut self) {
        unsafe { self.ctx.execute() };
    }

    /// 处理系统调用，返回调度事件
    pub fn handle_syscall(&mut self) -> SchedulingEvent {
        use tg_syscall::{SyscallId as Id, SyscallResult as Ret};
        use SchedulingEvent as Event;

        let id = self.ctx.a(7).into();
        let args = [
            self.ctx.a(0),
            self.ctx.a(1),
            self.ctx.a(2),
            self.ctx.a(3),
            self.ctx.a(4),
            self.ctx.a(5),
        ];

        // 统计系统调用次数（ID 在范围内才统计）
        let id_num: usize = self.ctx.a(7);
        if id_num < SYSCALL_COUNT {
            self.syscall_counts[id_num] += 1;
        }

        match tg_syscall::handle(Caller { entity: self as *mut _ as usize, flow: 0 }, id, args) {
            Ret::Done(ret) => match id {
                Id::EXIT => Event::Exit(self.ctx.a(0)),
                Id::SCHED_YIELD => {
                    *self.ctx.a_mut(0) = ret as _;
                    self.ctx.move_next();
                    Event::Yield
                }
                _ => {
                    *self.ctx.a_mut(0) = ret as _;
                    self.ctx.move_next();
                    Event::None
                }
            },
            Ret::Unsupported(_) => Event::UnsupportedSyscall(id),
        }
    }
}
