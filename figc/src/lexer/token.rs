use std::fmt::Display;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Ident(String),
    Int(String),
    Float(String),

    Illegal,
    Eof,
    Assign,

    Mod,
    Bang,
    Minus,
    ForwardSlash,
    Asterisk,
    Equal,
    NotEqual,
    LessThan,
    GreaterThan,

    Plus,
    Comma,
    Period,
    Semicolon,
    Colon,
    Lparen,
    Rparen,
    LSquirly,
    RSquirly,
    LBrack,
    RBrack,

    Function,
    Struct,
    External,
    Builtin,
    Let,
    Const,
    Loop,
    Export,
    Break,
    Import,

    If,
    Else,
    Return,
    True,
    False,

    String(String),
    Char(char),

    Ref,
}

impl Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Token::*;
        match self {
            Ident(str) => write!(f, "{}", str),
            Int(int) => write!(f, "{}", int),
            Float(fl) => write!(f, "{}", fl),

            Illegal => write!(f, "Illegal"),
            Eof => write!(f, "End of file"),
            Assign => write!(f, "="),

            Mod => write!(f, "%"),
            Bang => write!(f, "!"),
            Minus => write!(f, "-"),
            ForwardSlash => write!(f, "/"),
            Asterisk => write!(f, "*"),
            Equal => write!(f, "=="),
            NotEqual => write!(f, "!="),
            LessThan => write!(f, "<"),
            GreaterThan => write!(f, ">"),

            Plus => write!(f, "+"),
            Comma => write!(f, ","),
            Period => write!(f, "."),
            Semicolon => write!(f, ";"),
            Colon => write!(f, ":"),
            Lparen => write!(f, "("),
            Rparen => write!(f, ")"),
            LSquirly => write!(f, "{}", '{'),
            RSquirly => write!(f, "{}", '}'),
            LBrack => write!(f, "["),
            RBrack => write!(f, "]"),

            Function => write!(f, "fn"),
            Struct => write!(f, "struct"),
            Let => write!(f, "let"),
            Const => write!(f, "const"),

            Import => write!(f, "import"),

            If => write!(f, "if"),
            Else => write!(f, "else"),
            Return => write!(f, "return"),
            True => write!(f, "true"),
            False => write!(f, "false"),
            Export => write!(f, "export"),
            Loop => write!(f, "loop"),
            Break => write!(f, "break"),
            External => write!(f, "external"),
            Builtin => write!(f, "builtin"),

            String(s) => write!(f, "'{}'", s),
            Char(c) => write!(f, "'{}'", c),

            Ref => write!(f, "&"),
        }
    }
}
