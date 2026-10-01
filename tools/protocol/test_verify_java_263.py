import struct
import unittest

from verify_java_263 import unpack_item, vi


class ItemComponentPacketTests(unittest.TestCase):
    def test_managed_name_and_lore_components_and_removals(self):
        custom_name = b"\x08" + struct.pack(">H", 10) + b"menu title"
        lore = b"\x09\x08" + struct.pack(">iH", 1, 16) + b"menu description"
        body = (
            vi(1)
            + vi(5)
            + vi(2)
            + vi(1)
            + vi(6)
            + custom_name
            + vi(11)
            + lore
            + vi(3)
        )
        item, offset = unpack_item(body, 0)
        self.assertEqual(item, {"item_id": 5, "count": 1})
        self.assertEqual(offset, len(body))


if __name__ == "__main__":
    unittest.main()
