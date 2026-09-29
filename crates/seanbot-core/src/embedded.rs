//! 编译期内嵌的官方内容（内置知识库与官方技能），由 build.rs 生成。

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
