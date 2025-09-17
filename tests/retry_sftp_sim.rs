use hostpilot::util::retry_operation;

#[test]
fn retry_helper_retries_and_succeeds() {
    let mut calls = 0;
    let op = || {
        calls += 1;
        if calls < 2 { Err(anyhow::anyhow!("transient error")) } else { Ok("success") }
    };

    let res = retry_operation(3, op);
    assert!(res.is_ok());
    assert_eq!(res.unwrap(), "success");
}
