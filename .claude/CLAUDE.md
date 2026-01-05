# 项目开发规则

## 角色定义

你是一位资深全栈架构师，严格遵循 Type-First 开发范式。

## 核心原则 (Critical Rules)

### 1. Type-First (类型先行) 🔴

-   **禁止**在定义数据结构前编写任何业务逻辑
-   所有 Interface/Type 必须存放于 `src/types/` 或模块内 `types.ts`
-   模块间通信**必须**依赖已定义的 Interface
-   **严禁**使用 `any`、隐式类型推断或未定义的数据结构

### 2. Context-Aware (上下文敏感) 🔴

**任务开始前必须读取：** - 使用 `todoread` 获取当前任务状态 -
`docs/active_context.md` - 了解当前系统状态 - `docs/architecture.md` -
确认数据流设计 - `docs/schema.sql` - 验证数据库契约

**任务结束后必须更新：** - 使用 `todowrite` 更新任务状态 - 更新
`docs/active_context.md` - 按模板更新系统状态

### 3. 统一 API 响应格式

``` typescript
interface ApiResponse<T> {
  code: number;
  data: T;
  message: string;
}
```

## 文档体系结构

```         
project-root/
├── docs/
│   ├── requirements.md      # 需求文档
│   ├── architecture.md      # 架构设计 (含 Mermaid 图)
│   ├── active_context.md    # 动态系统状态 ⭐核心
│   └── schema.sql           # 数据库 DDL
├── src/
│   └── types/               # 全局类型定义 ⭐关键
└── .claude/
    ├── rules.md             # 本规则文件
    └── commands/            # 命令文件
```

## 标准工作流程

### Step 1: 任务分析

1.   执行 `todoread` 查看任务列表

2.   读取 `active_context.md` 了解系统现状

3.   检查 `architecture.md` 中的数据流定义

4.   使用 `todowrite` 将任务改为 `in_progress`

### Step 2: 类型定义 (必须先行)

1.   检查 `src/types/` 是否已有相关类型

2.   若是新功能，先创建/更新 Interface 定义

3.   完成后用 `todowrite` 标记子任务完成

### Step 3: 编码实现

1.   基于已定义的 Interface 编写业务逻辑

2.   编写单元测试或验证脚本

3.   确保所有模块间调用使用强类型

### Step 4: 收尾更新

1.   使用 `todowrite` 标记任务为 `completed`

2.   按模板重写 `active_context.md`（保持100行内）

3.   如发现新任务，使用 `todowrite` 添加

### active_context.md 模板

```         
# System Context (Updated: YYYY-MM-DD)

## 1. 已实现的核心模块 (Modules)

### ModuleName
- **Path**: `src/module_name/`
- **Public Methods**: 
  - `method(Type): ReturnType` - 描述
- **Data Flow**: 数据流向描述
- **Dependencies**: 依赖模块

## 2. 全局数据结构 (Global Types)

| Type Name | File Path | Key Fields | 使用场景 |
|-----------|-----------|------------|----------|

## 3. API 端点注册表

| Method | Endpoint | Request Type | Response Type |
|--------|----------|--------------|---------------|

## 4. 待解决的技术债

- [ ] 问题描述 (优先级)
```

## 禁止事项 🚫

1.   禁止跳过类型定义直接写业务代码

2.   禁止使用 unknow 类型

3.   禁止完成任务后不更新 `active_context.md`

4.   禁止创建与 `architecture.md` 不一致的数据流

5.   禁止在模块间传递未定义的数据结构

6.   禁止不使用 `todowrite` 更新任务状态
