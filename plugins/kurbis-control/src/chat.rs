use pumpkin_plugin_api::{common::NamedColor, text::TextComponent};

/// Keep the login URL visible for manual copy while making its entire text an
/// explicit browser-opening action in Minecraft's chat component.
pub(crate) fn server_login_link(url: &str) -> TextComponent {
    let link = TextComponent::text(url)
        .click_open_url(url)
        .underlined(true)
        .color_named(NamedColor::Blue);

    TextComponent::text("One-use server login link (valid for 15 minutes):\n")
        .add_child(link)
        .add_text("\nIf clicking is blocked, copy the link into your browser.")
}
