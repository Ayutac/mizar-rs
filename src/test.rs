#[cfg(test)]
mod tests {
  use itertools::Itertools;
  use crate::accom::Accomodator;
  use crate::parser::MizParser;
  use crate::types::DirectiveKind::{Notations, Vocabularies};
  use crate::types::{Article, Constructors, Directives, OrdArticle, RequirementIndexes};
  use crate::MizPath;
  use crate::write::OWriteJson;

  #[test]
  fn article() {
    let article_lower = Article::from_lower(b"xboole_0").unwrap();
    let article_upper = Article::from_upper(b"XBOOLE_0").unwrap();
    assert_eq!(article_lower, article_upper);
    let article_short = Article::from_lower(b"ups").unwrap();
    let article_normal = Article::from_lower(b"ups\0\0\0\0\0").unwrap();
    assert_eq!(article_normal, article_short);
    assert_eq!("ups", article_normal.as_str());
  }

  #[test]
  #[should_panic]
  fn article_panic() {
    Article::from_lower(b"longer_than_8");
  }

  #[test]
  fn ord_article() {
    let mml_lar = std::fs::read_to_string("miz/mizshare/mml.lar").unwrap();
    let ordering = mml_lar.lines().collect_vec();
    let tarski_art = Article::from_lower(b"tarski").unwrap();
    let xboole_0_art = Article::from_lower(b"xboole_0").unwrap();
    let xboole_x_art = Article::from_lower(b"xboole_x").unwrap();
    let tarski = OrdArticle::new(&tarski_art, &ordering);
    let xboole_0 = OrdArticle::new(&xboole_0_art, &ordering);
    let xboole_x = OrdArticle::new(&xboole_x_art, &ordering);
    assert!(tarski.eq(&tarski));
    assert!(tarski.lt(&xboole_0));
    assert!(tarski.lt(&xboole_x));
    assert!(xboole_0.lt(&xboole_x));
  }

  #[test]
  fn directives_sort() {
    let mut dir = Directives::default();
    dir.0[Vocabularies].push((Default::default(), Article::from_lower(b"xboole_0").unwrap()));
    dir.0[Vocabularies].push((Default::default(), Article::from_lower(b"tarski").unwrap()));
    assert_eq!("xboole_0", dir.0[Vocabularies].get(0).unwrap().1.as_str());
    let mml_lar = std::fs::read_to_string("miz/mizshare/mml.lar").unwrap();
    let ordering = mml_lar.lines().collect_vec();
    dir.sort(false, &ordering);
    assert_eq!("tarski", dir.0[Vocabularies].get(0).unwrap().1.as_str());
  }

  #[test]
  fn miz_path() {
    let miz_path = MizPath::new("xboole_0").unwrap();
    assert!(miz_path.read_miz().is_ok());
    let miz_path = MizPath::new("xboole_x").unwrap();
    assert!(miz_path.read_miz().is_err());
  }

  #[test]
  fn miz_parser() {
    let miz_path = MizPath::new("xboole_0").unwrap();
    let content = miz_path.read_miz().unwrap();
    let mut parser = MizParser::new(miz_path.art, None, &content, OWriteJson(None));
    let mut directives = Directives::default();
    // compare EVL file
    parser.parse_env(&mut directives);
    assert_eq!(Article::from_lower(b"tarski").unwrap(), directives.0[Notations].get(1).unwrap().1);
    assert_eq!(Article::from_lower(b"tarski").unwrap(), directives.0[Vocabularies].get(1).unwrap().1);
    assert_eq!(Article::from_lower(b"xboole_0").unwrap(), directives.0[Vocabularies].get(2).unwrap().1);
    assert_eq!(Article::from_lower(b"matroid0").unwrap(), directives.0[Vocabularies].get(3).unwrap().1);
    assert_eq!(Article::from_lower(b"aofa_000").unwrap(), directives.0[Vocabularies].get(4).unwrap().1);
  }

  #[test]
  fn accom() {
    let miz_path = MizPath::new("xboole_0").unwrap();
    let content = miz_path.read_miz().unwrap();
    let mut parser = MizParser::new(miz_path.art, None, &content, OWriteJson(None));
    let mut directives = Directives::default();
    parser.parse_env(&mut directives);
    let mut acc = Accomodator::default();
    acc.dirs = directives;
    let mut con = Constructors::default();
    // compare ATR file
    assert!(acc.accom_constructors(&mut con).is_ok());
    let mut req = RequirementIndexes::default();
    // compare ERE file
    assert!(acc.accom_requirements(&con, &mut req).is_ok());
  }
}
