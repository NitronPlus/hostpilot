// use hostpilot::util::retry_operation_with_ctx; // replaced with fully qualified call below

#[test]
fn retry_helper_retries_and_succeeds() {
    let mut calls = 0;
    let op = || {
        calls += 1;
        if calls < 2 { Err(anyhow::anyhow!("transient error")) } else { Ok("success") }
    };

    let res = hostpilot::util::retry_operation_with_ctx(
        3,
        op,
        hostpilot::util::RetryPhase::DuringTransfer,
        "test:retry_sftp_sim",
    );
    assert!(res.is_ok());
    assert_eq!(res.unwrap(), "success");
}
