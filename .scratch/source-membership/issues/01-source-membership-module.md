# 01: SourceMembership 模块

**What to build:** 一个能回答"这条已存技能在当前 checkout 的哪里"的模块，返回 At / Moved（带置信度与候选）/ Gone。不接任何调用方，纯新增。

**Blocked by:** None（可立即开始）

**Status:** ready-for-agent

- [ ] `locate(stored_skill, checkout) -> Placement` 存在，三结局齐全
- [ ] 高置信：目录内容哈希等于 `content_hash`
- [ ] 低置信：目录 basename 或 SKILL.md frontmatter name 命中
- [ ] 多个低置信候选全部返回，不替调用方挑选
- [ ] 只在 checkout 内扫描，不跨仓库
- [ ] 表驱动测试，无网络、无真实 git
