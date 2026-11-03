//! Plain-language reasons why the link to another computer does not come up.
//!
//! Handshake failures used to land only in the debug log, so two PCs that could not talk
//! to each other (wrong password, different "require password" setting, address missing
//! from the other side's list, different protocol) looked simply "not connected".

/// One unresolved problem with one peer, shown in the Komputery tab and the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkIssue {
    /// Peer address as configured (outbound) or as seen on the wire (inbound).
    pub peer: String,
    pub text: String,
}

/// `err` is the failure as text (`{:#}` of an `anyhow::Error` or an I/O error);
/// `outbound` is `true` when this computer dialled the peer.
pub fn describe_link_failure(err: &str, outbound: bool) -> String {
    let e = err.to_lowercase();
    let text = if e.contains("auth failed") {
        if outbound {
            "drugi komputer odrzucił hasło. Sprawdź to samo hasło i przełącznik „Wymagaj hasła” na obu komputerach."
        } else {
            "odrzucono połączenie: złe hasło albo „Wymagaj hasła” ustawione inaczej niż na drugim komputerze."
        }
    } else if e.contains("peer password off") {
        "drugi komputer nie ma włączonego „Wymagaj hasła”, a ten tak. Włącz je z tym samym hasłem na obu komputerach."
    } else if e.contains("version mismatch") {
        "inna wersja protokołu. Użyj tej samej wersji programu na obu komputerach."
    } else if e.contains("not in accepted list") {
        "tego adresu nie ma na liście „Komputery”. Dodaj go tam albo zostaw listę pustą."
    } else if e.contains("handshake timeout") {
        "drugi komputer nie odpowiedział na uzgadnianie połączenia (zapora albo inny program na tym porcie)."
    } else if e.contains("refused") || e.contains("10061") {
        "na drugim komputerze nic nie nasłuchuje. Uruchom tam program i sprawdź port TCP."
    } else if e.contains("timed out") || e.contains("10060") {
        "drugi komputer nie odpowiada. Zapora blokuje TCP albo komputer jest wyłączony."
    } else if e.contains("unreachable") || e.contains("10051") || e.contains("10065") {
        "adres nieosiągalny. Sprawdź, czy oba komputery są w tej samej sieci."
    } else if outbound
        && (e.contains("eof") || e.contains("closed") || e.contains("reset") || e.contains("10054"))
    {
        "drugi komputer zamknął połączenie przed uzgodnieniem. Najczęściej nie ma na swojej liście „Komputery” adresu tego komputera."
    } else {
        return format!("połączenie nie powiodło się: {err}");
    };
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_problems_are_named_for_both_directions() {
        let inbound = describe_link_failure("auth failed from PC2", false);
        assert!(inbound.contains("Wymagaj hasła"), "{inbound}");
        let outbound = describe_link_failure("HelloAck denied: Some(\"auth failed\")", true);
        assert!(outbound.contains("odrzucił hasło"), "{outbound}");
    }

    #[test]
    fn a_peer_without_password_is_named() {
        let text = describe_link_failure("peer password off", true);
        assert!(text.contains("nie ma włączonego"), "{text}");
    }

    #[test]
    fn protocol_acl_and_timeouts_have_their_own_advice() {
        assert!(describe_link_failure("version mismatch", false).contains("wersji"));
        assert!(describe_link_failure("rejected 1.2.3.4:5: not in accepted list", false).contains("liście"));
        assert!(describe_link_failure("handshake timeout", true).contains("uzgadnianie"));
    }

    #[test]
    fn windows_connect_errors_are_recognised_by_code_not_by_language() {
        let timeout = "Próba połączenia nie powiodła się (os error 10060)";
        assert!(describe_link_failure(timeout, true).contains("nie odpowiada"));
        let refused = "Nie można nawiązać połączenia (os error 10061)";
        assert!(describe_link_failure(refused, true).contains("nic nie nasłuchuje"));
    }

    #[test]
    fn an_abrupt_close_after_our_hello_points_at_the_other_side_list() {
        let text = describe_link_failure("early eof", true);
        assert!(text.contains("liście „Komputery”"), "{text}");
        // an inbound EOF is just a probe or a port scan, not worth blaming anything
        assert!(describe_link_failure("early eof", false).starts_with("połączenie nie powiodło się"));
    }

    #[test]
    fn unknown_errors_keep_the_original_text() {
        assert_eq!(
            describe_link_failure("weird", true),
            "połączenie nie powiodło się: weird"
        );
    }
}
