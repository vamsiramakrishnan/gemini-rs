//! Integration test: verify all public types are accessible via the prelude.

use gemini_live::prelude::*;

#[test]
fn traits_are_accessible() {
    fn _codec<T: Codec>() {}
    fn _transport<T: Transport>() {}
    fn _auth<T: AuthProvider>() {}
    fn _writer<T: SessionWriter>() {}
    fn _reader<T: SessionReader>() {}
    fn _tool_provider<T: ToolProvider>() {}
}

#[test]
fn implementations_are_accessible() {
    let _ = JsonCodec;
    let _ = TungsteniteTransport::new();
    let _ = MockTransport::new();
    let _ = GoogleAIAuth::new("key");
    let _ = VertexAIAuth::new("project", "location", "token");
}

#[test]
fn error_types_are_accessible() {
    fn _we(_: WebSocketError) {}
    fn _se(_: SetupError) {}
    fn _ae(_: AuthError) {}
    fn _ce(_: CodecError) {}
}

#[test]
fn type_safety_additions() {
    let _r = Role::User;
    let _p = Platform::GoogleAI;
}

#[test]
fn existing_types_still_accessible() {
    let _model = GeminiModel::Gemini2_0FlashLive;
    let _voice = Voice::Puck;
    let _modality = Modality::Audio;
    let _content = Content::user("hello");
    let _part = Part::text("test");
}

#[test]
fn connect_builder_accessible() {
    let config = SessionConfig::new("key").model(GeminiModel::Gemini2_0FlashLive);
    let _builder = ConnectBuilder::new(config);
}
