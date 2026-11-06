//! Colour emoji for the emoji panel. Our own fonts have none, so this is the one place that
//! uses the system's Noto Color Emoji (a recommended package); without it the panel lists
//! names only. The font is mapped only while the panel is open, and glyphs are decoded a few
//! per frame, so opening it stays within the budget of a panel's first frame.
use std::collections::HashMap;
use std::sync::Arc;

use tiny_skia::{FilterQuality, Pixmap, PixmapMut, PixmapPaint, Transform};

const FONT_PATHS: [&str; 4] = [
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto-color-emoji/NotoColorEmoji.ttf",
    "/usr/share/fonts/google-noto-color-emoji-fonts/NotoColorEmoji.ttf",
];
/// Glyphs decoded per painted frame
const DECODES_PER_FRAME: u32 = 6;

#[derive(Default)]
pub struct EmojiCache {
    font: Option<Arc<memmap2::Mmap>>,
    tried: bool,
    /// Decoded at the size in use; None when the font has no such glyph
    glyphs: HashMap<char, Option<Pixmap>>,
    px: u32,
    budget: u32,
    /// Something was left for the next frame
    pub incomplete: bool,
}

impl EmojiCache {
    pub fn begin_frame(&mut self) {
        self.budget = DECODES_PER_FRAME;
        self.incomplete = false;
    }

    /// The panel closed: gives back the mapping and the pictures
    pub fn release(&mut self) {
        *self = Self::default();
    }

    fn map_font(&mut self) {
        if self.tried {
            return;
        }
        self.tried = true;
        self.font = FONT_PATHS.iter().find_map(|path| {
            let file = std::fs::File::open(path).ok()?;
            // SAFETY: a read-only mapping of a system font, parsed by a bounds-checking reader
            unsafe { memmap2::Mmap::map(&file).ok() }.map(Arc::new)
        });
    }

    fn decode(&self, ch: char, px: u32) -> Option<Pixmap> {
        let font = self.font.as_ref()?;
        let face = ttf_parser::Face::parse(font, 0).ok()?;
        let glyph = face.glyph_index(ch)?;
        let image = face.glyph_raster_image(glyph, u16::MAX)?;
        if image.format != ttf_parser::RasterImageFormat::PNG {
            return None;
        }
        let source = Pixmap::decode_png(image.data).ok()?;
        let mut out = Pixmap::new(px, px)?;
        let scale = px as f32 / source.width().max(source.height()) as f32;
        let (dx, dy) = ((px as f32 - source.width() as f32 * scale) * 0.5, (px as f32 - source.height() as f32 * scale) * 0.5);
        let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
        out.draw_pixmap(0, 0, source.as_ref(), &paint, Transform::from_scale(scale, scale).post_translate(dx, dy), None);
        Some(out)
    }

    /// Draws at physical (x, y); false when the font has nothing to show
    pub fn draw(&mut self, target: &mut PixmapMut, ch: char, x: i32, y: i32, px: u32) -> bool {
        self.map_font();
        if self.px != px {
            self.glyphs.clear();
            self.px = px;
        }
        if !self.glyphs.contains_key(&ch) {
            if self.budget == 0 {
                self.incomplete = true;
                return true;
            }
            self.budget -= 1;
            let glyph = self.decode(ch, px);
            self.glyphs.insert(ch, glyph);
        }
        let Some(Some(glyph)) = self.glyphs.get(&ch) else { return false };
        target.draw_pixmap(x, y, glyph.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        true
    }
}

/// Emoji with the words that find them, Russian then English
pub const TABLE: &[(char, &str)] = &[
    ('😀', "улыбка радость смех grin smile happy"),
    ('😂', "смех слёзы ржу joy laugh tears"),
    ('🤣', "хохот катаюсь rofl laugh"),
    ('😊', "улыбка довольный blush smile"),
    ('😍', "влюблён глаза сердца love heart eyes"),
    ('😘', "поцелуй kiss"),
    ('😉', "подмигивание wink"),
    ('😎', "крутой очки cool sunglasses"),
    ('🤔', "думаю хмм think hmm"),
    ('😐', "нейтрально neutral"),
    ('😴', "сон сплю sleep"),
    ('😢', "грусть слеза sad cry"),
    ('😭', "плач рыдаю sob cry"),
    ('😡', "злость гнев angry rage"),
    ('😱', "ужас страх крик scream fear"),
    ('🥳', "праздник вечеринка party celebrate"),
    ('🤯', "взрыв мозга шок mind blown"),
    ('🙃', "наоборот ирония upside irony"),
    ('🥲', "улыбка сквозь слёзы smile tear"),
    ('🤝', "рукопожатие сделка handshake deal"),
    ('👍', "класс да лайк палец thumbs up like yes"),
    ('👎', "плохо нет дизлайк thumbs down no"),
    ('👌', "окей хорошо ok"),
    ('✌', "победа мир peace victory"),
    ('🤞', "удача пальцы luck fingers crossed"),
    ('👏', "аплодисменты браво clap"),
    ('🙏', "спасибо пожалуйста молюсь thanks please pray"),
    ('💪', "сила мышца strong muscle"),
    ('👋', "привет пока рука hello bye wave"),
    ('🤷', "не знаю пожимаю shrug"),
    ('🤦', "рукалицо facepalm"),
    ('👀', "глаза смотрю eyes look"),
    ('🧠', "мозг brain"),
    ('❤', "сердце любовь красное heart love red"),
    ('💔', "разбитое сердце broken heart"),
    ('💙', "синее сердце blue heart"),
    ('💚', "зелёное сердце green heart"),
    ('🖤', "чёрное сердце black heart"),
    ('🔥', "огонь пожар круто fire hot"),
    ('✨', "блеск искры магия sparkles magic"),
    ('⭐', "звезда star"),
    ('🌟', "сияющая звезда glowing star"),
    ('⚡', "молния быстро lightning fast"),
    ('💯', "сто процентов hundred"),
    ('✅', "галочка готово check done"),
    ('❌', "крест нет ошибка cross no error"),
    ('❗', "восклицание важно exclamation"),
    ('❓', "вопрос question"),
    ('⚠', "внимание предупреждение warning"),
    ('🚀', "ракета запуск rocket launch"),
    ('🎉', "поздравляю хлопушка праздник tada party"),
    ('🎁', "подарок gift"),
    ('🏆', "кубок победа trophy win"),
    ('🪶', "перо пёрышко feather"),
    ('☕', "кофе кофеин чай coffee caffeine"),
    ('🍵', "чай tea"),
    ('🍺', "пиво beer"),
    ('🍕', "пицца pizza"),
    ('🍔', "бургер burger"),
    ('🍎', "яблоко apple"),
    ('🍰', "торт пирожное cake"),
    ('🐱', "кот кошка cat"),
    ('🐶', "собака пёс dog"),
    ('🦊', "лиса fox"),
    ('🐻', "медведь bear"),
    ('🐧', "пингвин линукс penguin linux"),
    ('🦄', "единорог unicorn"),
    ('🐍', "змея питон snake python"),
    ('🦀', "краб раст crab rust"),
    ('🌈', "радуга rainbow"),
    ('☀', "солнце sun"),
    ('🌙', "луна ночь moon night"),
    ('☁', "облако cloud"),
    ('❄', "снег снежинка snow"),
    ('🌧', "дождь rain"),
    ('🌲', "ёлка дерево tree"),
    ('🌸', "цветок сакура flower"),
    ('💻', "ноутбук компьютер laptop computer"),
    ('🖥', "монитор компьютер desktop monitor"),
    ('📱', "телефон phone"),
    ('⌨', "клавиатура keyboard"),
    ('🖱', "мышь mouse"),
    ('🔋', "батарея заряд battery"),
    ('🔌', "вилка питание plug power"),
    ('💡', "идея лампа idea bulb"),
    ('🔧', "ключ настройка wrench fix"),
    ('🐛', "баг жук bug"),
    ('🔒', "замок закрыто lock"),
    ('🔑', "ключ пароль key password"),
    ('📁', "папка folder"),
    ('📄', "документ файл document file"),
    ('📝', "заметка запись memo note"),
    ('📌', "кнопка закрепить pin"),
    ('📎', "скрепка clip"),
    ('📅', "календарь дата calendar"),
    ('⏰', "будильник время alarm clock"),
    ('🕐', "часы время clock"),
    ('🔍', "поиск лупа search"),
    ('📷', "камера фото camera photo"),
    ('🎵', "музыка нота music note"),
    ('🎮', "игра геймпад game"),
    ('🎬', "кино фильм movie"),
    ('📚', "книги books"),
    ('✏', "карандаш pencil"),
    ('💰', "деньги мешок money"),
    ('💳', "карта оплата card pay"),
    ('🛒', "корзина покупки cart shopping"),
    ('🚗', "машина car"),
    ('✈', "самолёт plane"),
    ('🏠', "дом home house"),
    ('🌍', "земля мир earth world"),
    ('🗑', "корзина мусор trash"),
    ('💤', "сон zzz sleep"),
    ('🤖', "робот бот ларвис robot bot"),
    ('👻', "привидение ghost"),
    ('💀', "череп skull"),
    ('💩', "какашка poop"),
];
