pub fn get_pi_user_agent(version: &str) -> String { format!("mhc/{version} ({}; native; {})", std::env::consts::OS, std::env::consts::ARCH) }
