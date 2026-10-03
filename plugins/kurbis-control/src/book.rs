use anyhow::Result;
use pumpkin_plugin_api::{
    Item, ItemStack, ItemStackExt, Player,
    common::Hand,
    text::TextComponent,
    data_components::DataComponent,
};

const MAX_BOOK_PAGES: usize = 100;
const MAX_PAGE_BYTES: usize = 32_767;

/// Opens a temporary written book in an empty hand so the client can read it
/// without overwriting an item the player is carrying.
pub(crate) fn open_pages(player: &Player, title: &str, pages: &[String]) -> Result<()> {
    let pages = normalize_pages(pages);
    let hand = empty_hand(player)?;
    let book = ItemStack::of(Item::WrittenBook, 1);
    book.set_component(
        DataComponent::WrittenBookContent,
        &encode_written_book(title, "Kurbis", &pages),
    );
    player.set_item_in_hand(hand, Some(book));
    player.open_book(hand);
    Ok(())
}

/// Opens a writable book containing the current value and returns the hand/item
/// pair that should be restored after [`PlayerEditBookEvent`] is handled.
pub(crate) fn open_text_input(player: &Player, current: &str) -> Result<(Hand, Option<ItemStack>)> {
    let (hand, original) = empty_hand_with_original(player)?;
    let book = ItemStack::of(Item::WritableBook, 1);
    book.set_component(
        DataComponent::WritableBookContent,
        &encode_writable_book(&[current]),
    );
    player.set_item_in_hand(hand, Some(book));
    player.open_book(hand);
    Ok((hand, original))
}

fn empty_hand(player: &Player) -> Result<Hand> {
    Ok(empty_hand_with_original(player)?.0)
}

fn empty_hand_with_original(player: &Player) -> Result<(Hand, Option<ItemStack>)> {
    let right = player.get_item_in_hand(Hand::Right);
    if right.is_none() {
        return Ok((Hand::Right, right));
    }
    let left = player.get_item_in_hand(Hand::Left);
    if left.is_none() {
        return Ok((Hand::Left, left));
    }
    anyhow::bail!("hold an empty hand before opening a Kurbis book")
}

fn normalize_pages(pages: &[String]) -> Vec<String> {
    let mut pages = pages
        .iter()
        .flat_map(|page| split_page(page))
        .take(MAX_BOOK_PAGES)
        .collect::<Vec<_>>();
    if pages.is_empty() {
        pages.push("No entries.".to_owned());
    }
    pages
}

fn split_page(value: &str) -> impl Iterator<Item = String> {
    let mut pages = Vec::new();
    let mut page = String::new();
    for character in value.chars() {
        if !page.is_empty() && page.len() + character.len_utf8() > MAX_PAGE_BYTES {
            pages.push(std::mem::take(&mut page));
        }
        page.push(character);
    }
    if !page.is_empty() {
        pages.push(page);
    }
    pages.into_iter()
}

fn encode_written_book(title: &str, author: &str, pages: &[String]) -> Vec<u8> {
    let mut encoded = Vec::new();
    write_string(&mut encoded, title);
    encoded.push(0); // no filtered title
    write_string(&mut encoded, author);
    write_varint(&mut encoded, 0); // generation
    write_varint(&mut encoded, pages.len() as i32);
    for page in pages {
        encoded.extend_from_slice(&TextComponent::text(page).encode());
        encoded.push(0); // no filtered page
    }
    encoded.push(1); // resolved
    encoded
}

fn encode_writable_book(pages: &[&str]) -> Vec<u8> {
    let mut encoded = Vec::new();
    write_varint(&mut encoded, pages.len() as i32);
    for page in pages {
        write_string(&mut encoded, page);
        encoded.push(0); // no filtered page
    }
    encoded
}

fn write_string(output: &mut Vec<u8>, value: &str) {
    write_varint(output, value.len() as i32);
    output.extend_from_slice(value.as_bytes());
}

fn write_varint(output: &mut Vec<u8>, mut value: i32) {
    loop {
        if value & !0x7f == 0 {
            output.push(value as u8);
            return;
        }
        output.push((value as u8 & 0x7f) | 0x80);
        value = ((value as u32) >> 7) as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_encoding_matches_minecraft_wire_format() {
        let mut bytes = Vec::new();
        write_varint(&mut bytes, 0);
        write_varint(&mut bytes, 127);
        write_varint(&mut bytes, 128);
        write_varint(&mut bytes, 255);
        assert_eq!(bytes, [0, 127, 0x80, 1, 0xff, 1]);
    }

    #[test]
    fn empty_books_have_a_readable_fallback_page() {
        assert_eq!(normalize_pages(&[]), vec!["No entries."]);
    }
}
