# TODO

- [ ] `check_icon_cache` 增加文件大小校验：PNG < 2KB 视为损坏，自动删除并返回 `None`，触发重新提取。防止 `lockFocus` 失败等异常情况产生的半截文件导致图标永久不显示。
