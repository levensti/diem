script {
    use MessageAddress::Message;

    fun set_message(account: signer, message: vector<u8>) {
        Message::set_message(account, message);
    }
}
