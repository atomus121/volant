fn main() {
    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Google\Chrome\NativeMessagingHosts",
            "/s",
        ])
        .output();
    match out {
        Ok(o) => println!(
            "query status={} len_out={} len_err={}",
            o.status,
            o.stdout.len(),
            o.stderr.len()
        ),
        Err(e) => println!("query spawn err: {e}"),
    }
    let out = std::process::Command::new("reg")
        .args([
            "add",
            r"HKCU\Software\Google\Chrome\NativeMessagingHosts\probe.test",
            "/ve",
            "/t",
            "REG_SZ",
            "/d",
            "\"C:\\x y\\m.json\"",
            "/f",
        ])
        .output();
    match out {
        Ok(o) => println!(
            "add status={} out={} err={}",
            o.status,
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => println!("add spawn err: {e}"),
    }
    let out = std::process::Command::new("cmd")
        .args(["/c", r"reg add HKCU\Software\Google\Chrome\NativeMessagingHosts\probe2.test /ve /t REG_SZ /d zzz /f"])
        .output();
    match out {
        Ok(o) => println!(
            "cmd-reg status={} out={}",
            o.status,
            String::from_utf8_lossy(&o.stdout)
        ),
        Err(e) => println!("cmd-reg spawn err: {e}"),
    }
}
