//! 身份演示：生成本机机器码 + 构造 FQN。
//!
//! 运行：`cargo run -p nsmt-core --example identity`

use nsmt_core::identity::{generate_machine_id, AgentTag, Fqn, UserDomain};
use std::str::FromStr;

fn main() {
    let (machine_id, stable) = generate_machine_id();
    println!("machine_id = {machine_id}  (stable={stable})");

    let user_domain = UserDomain::new("ser163").expect("valid domain");
    let agent_tag = AgentTag::new("maka").expect("valid tag");
    let fqn = Fqn {
        user_domain,
        machine_id,
        agent_tag,
    };
    println!("fqn        = {fqn}");

    // 解析回验
    let parsed = Fqn::from_str(&fqn.to_string()).expect("roundtrip");
    assert_eq!(parsed, fqn);
    println!("roundtrip  = OK");
}
