use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct TestStruct {
    first: String,
    second: String,
    third: String,
}

#[perlmod::package(name = "TestLib::List", lib = "testlib")]
mod export {
    use anyhow::{Error, bail};
    use perlmod::Value;

    #[export]
    fn maybe_struct(#[list] all: Vec<Value>) -> Result<String, Error> {
        if all.is_empty() {
            bail!("empty list");
        }

        let test = match all.len() {
            1 => perlmod::de::from_ref_value(&all[0])?,
            3 => super::TestStruct {
                first: perlmod::de::from_ref_value(&all[0])?,
                second: perlmod::de::from_ref_value(&all[1])?,
                third: perlmod::de::from_ref_value(&all[2])?,
            },
            n => bail!("expected 1 or 3 parameters, got {n}"),
        };

        Ok(format!(
            "[{}] [{}] [{}]",
            test.first, test.second, test.third
        ))
    }
}
