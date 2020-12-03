//! main structures to interact with Biscuit tokens
use super::crypto::{KeyPair, PublicKey};
use super::datalog::{Fact, Rule, Caveat, SymbolTable, World, ID};
use super::error;
use super::format::SerializedBiscuit;
use builder::{BiscuitBuilder, BlockBuilder};
use prost::Message;
use rand_core::{CryptoRng, RngCore};
use std::collections::{HashMap, HashSet};

use crate::format::{convert::proto_block_to_token_block, schema};
use verifier::Verifier;

pub mod builder;
pub mod sealed;
pub mod verifier;

/// some symbols are predefined and available in every implementation, to avoid
/// transmitting them with every token
pub fn default_symbol_table() -> SymbolTable {
    let mut syms = SymbolTable::new();
    syms.insert("authority");
    syms.insert("ambient");
    syms.insert("resource");
    syms.insert("operation");
    syms.insert("right");
    syms.insert("current_time");
    syms.insert("revocation_id");

    syms
}

/// This structure represents a valid Biscuit token
///
/// It contains multiple `Block` elements, the associated symbol table,
/// and a serialized version of this data
///
/// ```rust
/// extern crate rand;
/// extern crate biscuit_auth as biscuit;
///
/// use biscuit::{crypto::KeyPair, token::{Biscuit, builder::*}};
///
/// fn main() {
///   let mut rng = rand::thread_rng();
///
///   let root = KeyPair::new(&mut rng);
///
///   // first we define the authority block for global data,
///   // like access rights
///   // data from the authority block cannot be created in any other block
///   let mut builder = Biscuit::builder(&root);
///   builder.add_authority_fact(fact("right", &[s("authority"), string("/a/file1.txt"), s("read")]));
///
///   // facts and rules can also be parsed from a string
///   builder.add_authority_fact("right(#authority, \"/a/file1.txt\", #read)").expect("parse error");
///
///   let token1 = builder.build(&mut rng).unwrap();
///
///   // we can create a new block builder from that token
///   let mut builder2 = token1.create_block();
///   builder2.check_operation("read");
///
///   let keypair2 = KeyPair::new(&mut rng);
///   let token2 = token1.append(&mut rng, &keypair2, builder2).unwrap();
/// }
/// ```
#[derive(Clone, Debug)]
pub struct Biscuit {
    authority: Block,
    blocks: Vec<Block>,
    symbols: SymbolTable,
    container: Option<SerializedBiscuit>,
}

impl Biscuit {
    /// creates a new token
    ///
    /// the public part of the root keypair must be used for verification
    ///
    /// The block is an authority block: its index must be 0 and all of its facts must have the authority tag
    pub fn new<T: RngCore + CryptoRng>(
        rng: &mut T,
        root: &KeyPair,
        mut symbols: SymbolTable,
        authority: Block,
    ) -> Result<Biscuit, error::Token> {
        let h1 = symbols.symbols.iter().collect::<HashSet<_>>();
        let h2 = authority.symbols.symbols.iter().collect::<HashSet<_>>();

        if !h1.is_disjoint(&h2) {
            return Err(error::Token::SymbolTableOverlap);
        }

        if authority.index as usize != 0 {
            return Err(error::Token::InvalidAuthorityIndex(authority.index));
        }

        symbols
            .symbols
            .extend(authority.symbols.symbols.iter().cloned());

        let blocks = vec![];

        let container =
            SerializedBiscuit::new(rng, root, &authority).map_err(error::Token::Format)?;

        Ok(Biscuit {
            authority,
            blocks,
            symbols,
            container: Some(container),
        })
    }

    /// deserializes a token and validates the signature using the root public key
    pub fn from(slice: &[u8]) -> Result<Self, error::Token> {
      Biscuit::from_with_symbols(slice, default_symbol_table())
    }

    /// deserializes a token and validates the signature using the root public key, with a custom symbol table
    pub fn from_with_symbols(slice: &[u8], mut symbols: SymbolTable) -> Result<Self, error::Token> {
        let container = SerializedBiscuit::from_slice(slice).map_err(error::Token::Format)?;

        let authority: Block = schema::Block::decode(&container.authority[..])
            .map_err(|e| {
                error::Token::Format(error::Format::BlockDeserializationError(format!(
                    "error deserializing authority block: {:?}",
                    e
                )))
            })
            .and_then(|b| proto_block_to_token_block(&b).map_err(error::Token::Format))?;

        if authority.index != 0 {
            return Err(error::Token::InvalidAuthorityIndex(authority.index));
        }

        let mut blocks = vec![];

        let mut index = 1;
        for block in container.blocks.iter() {
            let deser: Block = schema::Block::decode(&block[..])
                .map_err(|e| {
                    error::Token::Format(error::Format::BlockDeserializationError(format!(
                        "error deserializing block: {:?}",
                        e
                    )))
                })
                .and_then(|b| proto_block_to_token_block(&b).map_err(error::Token::Format))?;

            if deser.index != index {
                return Err(error::Token::InvalidBlockIndex(error::InvalidBlockIndex {
                    expected: index,
                    found: deser.index,
                }));
            }
            blocks.push(deser);

            index += 1;
        }

        symbols
            .symbols
            .extend(authority.symbols.symbols.iter().cloned());

        for block in blocks.iter() {
            symbols
                .symbols
                .extend(block.symbols.symbols.iter().cloned());
        }

        let container = Some(container);

        Ok(Biscuit {
            authority,
            blocks,
            symbols,
            container,
        })
    }

    /// deserializes a sealed token and checks its signature with the secret, using a custom symbol table
    pub fn from_sealed(slice: &[u8], secret: &[u8]) -> Result<Self, error::Token> {
      Biscuit::from_sealed_with_symbols(slice, secret, default_symbol_table())
    }

    /// deserializes a sealed token and checks its signature with the secret
    pub fn from_sealed_with_symbols(slice: &[u8], secret: &[u8], mut symbols: SymbolTable) -> Result<Self, error::Token> {
        let container =
            sealed::SealedBiscuit::from_slice(slice, secret).map_err(error::Token::Format)?;

        let authority: Block = schema::Block::decode(&container.authority[..])
            .map_err(|e| {
                error::Token::Format(error::Format::BlockDeserializationError(format!(
                    "error deserializing authority block: {:?}",
                    e
                )))
            })
            .and_then(|b| proto_block_to_token_block(&b).map_err(error::Token::Format))?;

        if authority.index != 0 {
            return Err(error::Token::InvalidAuthorityIndex(authority.index));
        }

        let mut blocks = vec![];

        let mut index = 1;
        for block in container.blocks.iter() {
            let deser: Block = schema::Block::decode(&block[..])
                .map_err(|e| {
                    error::Token::Format(error::Format::BlockDeserializationError(format!(
                        "error deserializing block: {:?}",
                        e
                    )))
                })
                .and_then(|b| proto_block_to_token_block(&b).map_err(error::Token::Format))?;

            if deser.index != index {
                return Err(error::Token::InvalidBlockIndex(error::InvalidBlockIndex {
                    expected: index,
                    found: deser.index,
                }));
            }
            blocks.push(deser);

            index += 1;
        }

        symbols
            .symbols
            .extend(authority.symbols.symbols.iter().cloned());

        for block in blocks.iter() {
            symbols
                .symbols
                .extend(block.symbols.symbols.iter().cloned());
        }

        let container = None;

        Ok(Biscuit {
            authority,
            blocks,
            symbols,
            container,
        })
    }

    /// serializes the token
    pub fn to_vec(&self) -> Result<Vec<u8>, error::Token> {
        match self.container.as_ref() {
            None => Err(error::Token::InternalError),
            Some(c) => c.to_vec().map_err(error::Token::Format),
        }
    }

    /// serializes the token
    pub fn serialized_size(&self) -> Result<usize, error::Token> {
        match self.container.as_ref() {
            None => Err(error::Token::InternalError),
            Some(c) => Ok(c.serialized_size()),
        }
    }

    /// serializes the token
    pub fn sealed_size(&self) -> Result<usize, error::Token> {
        // FIXME: not ideal to serialize a sealed token just for this
        let sealed =
            sealed::SealedBiscuit::from_token(self, &b"ABCD"[..]).map_err(error::Token::Format)?;
        Ok(sealed.serialized_size())
    }

    /// serializes a sealed version of the token
    pub fn seal(&self, secret: &[u8]) -> Result<Vec<u8>, error::Token> {
        let sealed =
            sealed::SealedBiscuit::from_token(self, secret).map_err(error::Token::Format)?;
        sealed.to_vec().map_err(error::Token::Format)
    }

    /// returns the internal representation of the token
    pub fn container(&self) -> Option<&SerializedBiscuit> {
        self.container.as_ref()
    }

    pub fn check_root_key(&self, root: PublicKey) -> Result<(), error::Token> {
        self.container
            .as_ref()
            .map(|c| c.check_root_key(root).map_err(error::Token::Format))
            .unwrap_or(Err(error::Token::Sealed))?;
        Ok(())
    }

    pub fn verify(&self, root: PublicKey) -> Result<Verifier, error::Token> {
        self.check_root_key(root)?;
        Verifier::new(self).map_err(error::Token::FailedLogic)
    }

    pub fn verify_sealed(&self) -> Result<Verifier, error::Token> {
        if self.container.is_some() {
            Err(error::Token::InternalError)
        } else {
            Verifier::new(self).map_err(error::Token::FailedLogic)
        }
    }

    pub(crate) fn generate_world(&self, symbols: &SymbolTable) -> Result<World, error::Logic> {
        let mut world = World::new();

        let authority_index = symbols.get("authority").unwrap();
        let ambient_index = symbols.get("ambient").unwrap();

        for fact in self.authority.facts.iter().cloned() {
            if fact.predicate.ids[0] == ID::Symbol(ambient_index) {
                return Err(error::Logic::InvalidAuthorityFact(
                    symbols.print_fact(&fact),
                ));
            }

            world.facts.insert(fact);
        }

        for rule in self.authority.rules.iter().cloned() {
            world.rules.push(rule);
        }

        for (i, block) in self.blocks.iter().enumerate() {
            // blocks cannot provide authority or ambient facts
            for fact in block.facts.iter().cloned() {
                if fact.predicate.ids[0] == ID::Symbol(authority_index)
                    || fact.predicate.ids[0] == ID::Symbol(ambient_index)
                {
                    return Err(error::Logic::InvalidBlockFact(
                        i as u32,
                        symbols.print_fact(&fact),
                    ));
                }

                world.facts.insert(fact);
            }

            for rule in block.rules.iter().cloned() {
                // block rules cannot generate authority or ambient facts
                if rule.head.ids[0] == ID::Symbol(authority_index)
                    || rule.head.ids[0] == ID::Symbol(ambient_index)
                {
                    return Err(error::Logic::InvalidBlockRule(
                        i as u32,
                        symbols.print_rule(&rule),
                    ));
                }
                world.rules.push(rule);
            }
        }

        world.run();

        Ok(world)
    }

    pub(crate) fn caveats(&self) -> Vec<Vec<Caveat>> {
        let mut result = Vec::new();
        let v = self.authority.caveats.to_vec();
        result.push(v);

        for block in self.blocks.iter() {
            let v = block.caveats.to_vec();
            result.push(v);
        }

        result
    }

    /// checks the caveats of a token, in the context of the request it comes with
    ///
    /// the verifier provides ambient facts (that must carry the "ambient" tag) like
    /// which resource is requested, which operation, the current time, etc
    ///
    /// those ambient facts can also be generated by the provided ambient rules
    ///
    /// the verifier can also provide its own caveats to validate the content of the token.
    /// Verifier caveats can either apply on the "authority" part (they will be tested once
    /// in the entire token), while block level caveast will be tested once per block.
    ///
    /// the symbol table argument is generated from the token's symbol table, adding
    /// new symbols as needed from ambient facts and rules
    ///
    /// if successful, it returns answers to the verifier queries as a HashMap indexed
    /// by the query name. Each query result contains a HashMap of block id -> Vec of Facts
    pub(crate) fn check(
        &self,
        symbols: &SymbolTable,
        mut ambient_facts: Vec<Fact>,
        ambient_rules: Vec<Rule>,
        verifier_caveats: Vec<Caveat>,
        queries: HashMap<String, Rule>,
    ) -> Result<HashMap<String, Vec<Fact>>, error::Logic> {
        let mut world = self.generate_world(symbols)?;

        for fact in ambient_facts.drain(..) {
            world.facts.insert(fact);
        }

        for rule in ambient_rules.iter().cloned() {
            world.rules.push(rule);
        }

        world.run();
        //println!("world:\n{}", symbols.print_world(&world));

        // we only keep the verifier rules
        //world.rules = ambient_rules;

        let mut errors = vec![];

        // authority caveats provided by the authority block
        for (i, caveat) in self.authority.caveats.iter().enumerate() {
            let mut successful = false;

            for query in caveat.queries.iter() {
                let res = world.query_rule(query.clone());
                if !res.is_empty() {
                    successful = true;
                    break;
                }
            }

            if !successful {
                errors.push(error::FailedCaveat::Block(error::FailedBlockCaveat {
                    block_id: 0,
                    caveat_id: i as u32,
                    rule: symbols.print_caveat(caveat),
                }));
            }
        }

        // verifier caveats
        for (i, caveat) in verifier_caveats.iter().enumerate() {
            let mut successful = false;

            for query in caveat.queries.iter() {
                let res = world.query_rule(query.clone());
                if !res.is_empty() {
                    successful = true;
                    break;
                }
            }

            if !successful {
                errors.push(error::FailedCaveat::Verifier(error::FailedVerifierCaveat {
                    caveat_id: i as u32,
                    rule: symbols.print_caveat(caveat),
                }));
            }
        }

        for (i, block) in self.blocks.iter().enumerate() {
            for (j, caveat) in block.caveats.iter().enumerate() {
                let mut successful = false;

                for query in caveat.queries.iter() {
                    let res = world.query_rule(query.clone());
                    if !res.is_empty() {
                        successful = true;
                        break;
                    }
                }

                if !successful {
                    errors.push(error::FailedCaveat::Block(error::FailedBlockCaveat {
                        block_id: i as u32,
                        caveat_id: j as u32,
                        rule: symbols.print_caveat(caveat),
                    }));
                }
            }
        }

        let mut query_results = HashMap::new();
        for (name, rule) in queries.iter() {
            let res = world.query_rule(rule.clone());
            query_results.insert(name.clone(), res);
        }

        if errors.is_empty() {
            Ok(query_results)
        } else {
            Err(error::Logic::FailedCaveats(errors))
        }
    }

    pub fn builder<'a>(
        root: &'a KeyPair,
    ) -> BiscuitBuilder<'a> {
        Biscuit::builder_with_symbols(root, default_symbol_table())
    }

    pub fn builder_with_symbols<'a>(
        root: &'a KeyPair,
        symbols: SymbolTable,
    ) -> BiscuitBuilder<'a> {
        BiscuitBuilder::new(root, symbols)
    }

    /// creates a new block builder
    pub fn create_block(&self) -> BlockBuilder {
        BlockBuilder::new((1 + self.blocks.len()) as u32)
    }

    /// adds a new block to the token
    ///
    /// since the public key is integrated into the token, the keypair can be
    /// discarded right after calling this function
    pub fn append<T: RngCore + CryptoRng>(
        &self,
        rng: &mut T,
        keypair: &KeyPair,
        block_builder: BlockBuilder,
    ) -> Result<Self, error::Token> {
        if self.container.is_none() {
            return Err(error::Token::Sealed);
        }

        let block = block_builder.build(self.symbols.clone());

        let h1 = self.symbols.symbols.iter().collect::<HashSet<_>>();
        let h2 = block.symbols.symbols.iter().collect::<HashSet<_>>();

        if !h1.is_disjoint(&h2) {
            return Err(error::Token::SymbolTableOverlap);
        }

        if block.index as usize != 1 + self.blocks.len() {
            return Err(error::Token::InvalidBlockIndex(error::InvalidBlockIndex {
                expected: 1 + self.blocks.len() as u32,
                found: block.index,
            }));
        }

        let authority = self.authority.clone();
        let mut blocks = self.blocks.clone();
        let mut symbols = self.symbols.clone();

        let container = match self.container.as_ref() {
            None => return Err(error::Token::Sealed),
            Some(c) => c
                .append(rng, keypair, &block)
                .map_err(error::Token::Format)?,
        };

        symbols
            .symbols
            .extend(block.symbols.symbols.iter().cloned());
        blocks.push(block);

        Ok(Biscuit {
            authority,
            blocks,
            symbols,
            container: Some(container),
        })
    }

    pub fn context(&self) -> Vec<Option<String>> {
      let mut res = vec![];
      res.push(self.authority.context.clone());

      for b in self.blocks.iter() {
        res.push(b.context.clone());
      }

      res
    }

    /// list blocks
    pub fn get_blocks(&self) -> &Vec<Block> {
        &self.blocks
    }

    /// get authority block
    pub fn get_authority(&self) -> &Block {
        &self.authority
    }

    /// pretty printer for this token
    pub fn print(&self) -> String {
        let authority = print_block(&self.symbols, &self.authority);
        let blocks: Vec<_> = self
            .blocks
            .iter()
            .map(|b| print_block(&self.symbols, b))
            .collect();

        format!(
            "Biscuit {{\n    symbols: {:?}\n    authority: {}\n    blocks: [\n        {}\n    ]\n}}",
            self.symbols.symbols,
            authority,
            blocks.join(",\n\t")
        )
    }
}

fn print_block(symbols: &SymbolTable, block: &Block) -> String {
    let facts: Vec<_> = block.facts.iter().map(|f| symbols.print_fact(f)).collect();
    let rules: Vec<_> = block.rules.iter().map(|r| symbols.print_rule(r)).collect();
    let caveats: Vec<_> = block
        .caveats
        .iter()
        .map(|r| symbols.print_caveat(r))
        .collect();


    let facts = if facts.is_empty() {
      String::new()
    } else {
      format!("\n                {}\n            ", facts.join(",\n                "))
    };
    let rules = if rules.is_empty() {
      String::new()
    } else {
      format!("\n                {}\n            ", rules.join(",\n                "))
    };
    let caveats = if caveats.is_empty() {
      String::new()
    } else {
      format!("\n                {}\n            ", caveats.join(",\n                "))
    };

    format!(
        "Block[{}] {{\n            symbols: {:?}\n            context: \"{}\"\n            facts: [{}]\n            rules: [{}]\n            caveats: [{}]\n        }}",
        block.index,
        block.symbols.symbols,
        block.context.as_deref().unwrap_or(""),
        facts,
        rules,
        caveats,
    )
}

#[derive(Clone, Debug)]
pub struct Block {
    /// position of the block
    pub index: u32,
    /// list of symbols introduced by this block
    pub symbols: SymbolTable,
    /// list of facts provided by this block
    pub facts: Vec<Fact>,
    /// list of rules provided by this block
    pub rules: Vec<Rule>,
    /// caveats that the token and ambient data must validate
    pub caveats: Vec<Caveat>,
    /// contextual information that can be looked up before the verification
    /// (as an example, a user id to query rights into a database)
    pub context: Option<String>,
}

impl Block {
    /// creates a new block
    ///
    /// blocks should be created through the BlockBuilder interface instead, to avoid mistakes
    pub fn new(index: u32, base_symbols: SymbolTable) -> Block {
        Block {
            index,
            symbols: base_symbols,
            facts: vec![],
            rules: vec![],
            caveats: vec![],
            context: None,
        }
    }

    pub fn symbol_add(&mut self, s: &str) -> ID {
        self.symbols.add(s)
    }

    pub fn symbol_insert(&mut self, s: &str) -> u64 {
        self.symbols.insert(s)
    }

    fn check(
        &self,
        i: usize,
        mut world: World,
        symbols: &SymbolTable,
        verifier_caveats: &[Caveat],
        queries: &HashMap<String, Rule>,
        query_results: &mut HashMap<String, HashMap<u32, Vec<Fact>>>,
    ) -> Result<(), error::Logic> {
        let authority_index = symbols.get("authority").unwrap();
        let ambient_index = symbols.get("ambient").unwrap();

        for fact in self.facts.iter().cloned() {
            if fact.predicate.ids[0] == ID::Symbol(authority_index)
                || fact.predicate.ids[0] == ID::Symbol(ambient_index)
            {
                return Err(error::Logic::InvalidBlockFact(
                    i as u32,
                    symbols.print_fact(&fact),
                ));
            }

            world.facts.insert(fact);
        }

        for rule in self.rules.iter().cloned() {
            world.rules.push(rule);
        }

        world.run();

        let mut errors = vec![];
        for (j, caveat) in self.caveats.iter().enumerate() {
            let mut successful = false;

            for query in caveat.queries.iter() {
                let res = world.query_rule(query.clone());
                if !res.is_empty() {
                    successful = true;
                    break;
                }
            }

            if !successful {
                errors.push(error::FailedCaveat::Block(error::FailedBlockCaveat {
                    block_id: i as u32,
                    caveat_id: j as u32,
                    rule: symbols.print_caveat(caveat),
                }));
            }
        }

        for (j, caveat) in verifier_caveats.iter().enumerate() {
            let mut successful = false;

            for query in caveat.queries.iter() {
                let res = world.query_rule(query.clone());
                if !res.is_empty() {
                    successful = true;
                    break;
                }
            }

            if !successful {
                errors.push(error::FailedCaveat::Verifier(error::FailedVerifierCaveat {
                    caveat_id: j as u32,
                    rule: symbols.print_caveat(caveat),
                }));
            }
        }

        for (name, rule) in queries.iter() {
          let res = world.query_rule(rule.clone());
          if !res.is_empty() {
            let entry = query_results.entry(name.clone()).or_insert_with(HashMap::new);
            (*entry).insert(i as u32, res);
          }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(error::Logic::FailedCaveats(errors))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::builder::{fact, pred, rule, s, var, int};
    use super::*;
    use crate::crypto::KeyPair;
    use crate::error::*;
    use rand::prelude::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn basic() {
        let mut rng: StdRng = SeedableRng::seed_from_u64(0);
        let root = KeyPair::new(&mut rng);

        let serialized1 = {
            let mut builder = Biscuit::builder(&root);

            builder.add_authority_fact("right(#authority, #file1, #read)").unwrap();
            builder.add_authority_fact("right(#authority, #file2, #read)").unwrap();
            builder.add_authority_fact("right(#authority, #file1, #write)").unwrap();

            let biscuit1 = builder.build(&mut rng).unwrap();

            println!("biscuit1 (authority): {}", biscuit1.print());

            biscuit1.to_vec().unwrap()
        };

        //println!("generated biscuit token: {} bytes:\n{}", serialized1.len(), serialized1.to_hex(16));
        println!("generated biscuit token: {} bytes", serialized1.len());
        //panic!();

        /*
        for i in 0..9 {
            let biscuit1_deser = Biscuit::from(&serialized1, root.public).unwrap();

            // new caveat: can only have read access1
            let mut block2 = biscuit1_deser.create_block();

            block2.add_caveat(&rule(
                "caveat1",
                &[var(0)],
                &[
                    pred("resource", &[s("ambient"), var(0)]),
                    pred("operation", &[s("ambient"), s("read")]),
                    pred("right", &[s("authority"), var(0), s("read")]),
                ],
            ));

            let keypair2 = KeyPair::new(&mut rng);
            let biscuit2 = biscuit1_deser.append(&keypair2, block2.to_block()).unwrap();

            println!("biscuit2 (1 caveat): {}", biscuit2.print());

            serialized1 = biscuit2.to_vec().unwrap();

        }
        println!("generated biscuit token 2: {} bytes", serialized1.len());
        panic!();
        */

        let serialized2 = {
            let biscuit1_deser = Biscuit::from(&serialized1).unwrap();

            // new caveat: can only have read access1
            let mut block2 = biscuit1_deser.create_block();

            block2.add_caveat(rule(
                "caveat1",
                &[var("resource")],
                &[
                    pred("resource", &[s("ambient"), var("resource")]),
                    pred("operation", &[s("ambient"), s("read")]),
                    pred("right", &[s("authority"), var("resource"), s("read")]),
                ],
            )).unwrap();

            let keypair2 = KeyPair::new(&mut rng);
            let biscuit2 = biscuit1_deser
                .append(&mut rng, &keypair2, block2)
                .unwrap();

            println!("biscuit2 (1 caveat): {}", biscuit2.print());

            biscuit2.to_vec().unwrap()
        };

        //println!("generated biscuit token 2: {} bytes\n{}", serialized2.len(), serialized2.to_hex(16));
        println!("generated biscuit token 2: {} bytes", serialized2.len());

        let serialized3 = {
            let biscuit2_deser = Biscuit::from(&serialized2).unwrap();

            // new caveat: can only access file1
            let mut block3 = biscuit2_deser.create_block();

            block3.add_caveat(rule(
                "caveat2",
                &[s("file1")],
                &[pred("resource", &[s("ambient"), s("file1")])],
            )).unwrap();

            let keypair3 = KeyPair::new(&mut rng);
            let biscuit3 = biscuit2_deser
                .append(&mut rng, &keypair3, block3)
                .unwrap();

            biscuit3.to_vec().unwrap()
        };

        //println!("generated biscuit token 3: {} bytes\n{}", serialized3.len(), serialized3.to_hex(16));
        println!("generated biscuit token 3: {} bytes", serialized3.len());
        //panic!();

        let final_token = Biscuit::from(&serialized3).unwrap();
        final_token.check_root_key(root.public()).unwrap();
        println!("final token:\n{}", final_token.print());
        {
            let mut symbols = final_token.symbols.clone();

            let facts = vec![
                fact("resource", &[s("ambient"), s("file1")]),
                fact("operation", &[s("ambient"), s("read")]),
            ];
            let mut ambient_facts = vec![];

            for fact in facts.iter() {
                ambient_facts.push(fact.convert(&mut symbols));
            }

            //println!("final token: {:#?}", final_token);
            //println!("ambient facts: {:#?}", ambient_facts);
            let res = final_token.check(&symbols, ambient_facts, vec![], vec![], HashMap::new());
            println!("res1: {:?}", res);
            res.unwrap();
        }

        {
            let mut symbols = final_token.symbols.clone();

            let facts = vec![
                fact("resource", &[s("ambient"), s("file2")]),
                fact("operation", &[s("ambient"), s("write")]),
            ];
            let mut ambient_facts = vec![];

            for fact in facts.iter() {
                ambient_facts.push(fact.convert(&mut symbols));
            }

            let res = final_token.check(&symbols, ambient_facts, vec![], vec![], HashMap::new());
            println!("res2: {:#?}", res);
            assert_eq!(res,
              Err(Logic::FailedCaveats(vec![
                FailedCaveat::Block(FailedBlockCaveat { block_id: 0, caveat_id: 0, rule: String::from("*caveat1($resource) <- resource(#ambient, $resource), operation(#ambient, #read), right(#authority, $resource, #read)") }),
                FailedCaveat::Block(FailedBlockCaveat { block_id: 1, caveat_id: 0, rule: String::from("*caveat2(#file1) <- resource(#ambient, #file1)") })
              ])));
        }
    }

    #[test]
    fn folders() {
        let mut rng: StdRng = SeedableRng::seed_from_u64(0);
        let root = KeyPair::new(&mut rng);

        let mut builder = Biscuit::builder(&root);

        builder.add_right("/folder1/file1", "read");
        builder.add_right("/folder1/file1", "write");
        builder.add_right("/folder1/file2", "read");
        builder.add_right("/folder1/file2", "write");
        builder.add_right("/folder2/file3", "read");

        let biscuit1 = builder.build(&mut rng).unwrap();

        println!("biscuit1 (authority): {}", biscuit1.print());

        let mut block2 = biscuit1.create_block();

        block2.resource_prefix("/folder1/");
        block2.check_right("read");

        let keypair2 = KeyPair::new(&mut rng);
        let biscuit2 = biscuit1
            .append(&mut rng, &keypair2, block2)
            .unwrap();

        {
            let mut verifier = biscuit2.verify(root.public()).unwrap();
            verifier.add_resource("/folder1/file1");
            verifier.add_operation("read");

            let res = verifier.verify();
            println!("res1: {:?}", res);
            res.unwrap();
        }

        {
            let mut verifier = biscuit2.verify(root.public()).unwrap();
            verifier.add_resource("/folder2/file3");
            verifier.add_operation("read");

            let res = verifier.verify();
            println!("res2: {:?}", res);
            assert_eq!(
                res,
                Err(Token::FailedLogic(Logic::FailedCaveats(vec![
                    FailedCaveat::Block(FailedBlockCaveat {
                        block_id: 1,
                        caveat_id: 0,
                        rule: String::from(
                            "*prefix($resource) <- resource(#ambient, $resource) @ $resource matches /folder1/*"
                        )
                    }),
                ])))
            );
        }

        {
            let mut verifier = biscuit2.verify(root.public()).unwrap();
            verifier.add_resource("/folder2/file1");
            verifier.add_operation("write");

            let res = verifier.verify();
            println!("res3: {:?}", res);
            assert_eq!(res,
              Err(Token::FailedLogic(Logic::FailedCaveats(vec![
                FailedCaveat::Block(FailedBlockCaveat { block_id: 1, caveat_id: 0, rule: String::from("*prefix($resource) <- resource(#ambient, $resource) @ $resource matches /folder1/*") }),
                FailedCaveat::Block(FailedBlockCaveat { block_id: 1, caveat_id: 1, rule: String::from("*check_right(#read) <- resource(#ambient, $resource_name), operation(#ambient, #read), right(#authority, $resource_name, #read)") }),
              ]))));
        }
    }

    #[test]
    fn constraints() {
        let mut rng: StdRng = SeedableRng::seed_from_u64(0);
        let root = KeyPair::new(&mut rng);

        let mut builder = Biscuit::builder(&root);

        builder.add_right("file1", "read");
        builder.add_right("file2", "read");

        let biscuit1 = builder.build(&mut rng).unwrap();

        println!("biscuit1 (authority): {}", biscuit1.print());

        let mut block2 = biscuit1.create_block();

        block2.expiration_date(SystemTime::now() + Duration::from_secs(30));
        block2.revocation_id(1234);

        let keypair2 = KeyPair::new(&mut rng);
        let biscuit2 = biscuit1
            .append(&mut rng, &keypair2, block2)
            .unwrap();

        {
            let mut verifier = biscuit2.verify(root.public()).unwrap();
            verifier.add_resource("file1");
            verifier.add_operation("read");
            verifier.set_time();

            let res = verifier.verify();
            println!("res1: {:?}", res);
            res.unwrap();
        }

        {
            println!("biscuit2: {}", biscuit2.print());
            let mut verifier = biscuit2.verify(root.public()).unwrap();
            verifier.add_resource("file1");
            verifier.add_operation("read");
            verifier.set_time();
            verifier.revocation_check(&[0, 1, 2, 5, 1234]);

            let res = verifier.verify();
            println!("res3: {:?}", res);

            // error message should be like this:
            //"Verifier caveat 0 failed: revocation_check(0?) <- revocation_id(0?) | 0? not in {2, 1234, 1, 5, 0}"
            assert!(res.is_err());
        }
    }

    #[test]
    fn sealed_token() {
        let mut rng: StdRng = SeedableRng::seed_from_u64(0);
        let root = KeyPair::new(&mut rng);
        let mut builder = Biscuit::builder(&root);

        builder.add_right("/folder1/file1", "read");
        builder.add_right("/folder1/file1", "write");
        builder.add_right("/folder1/file2", "read");
        builder.add_right("/folder1/file2", "write");
        builder.add_right("/folder2/file3", "read");

        let biscuit1 = builder.build(&mut rng).unwrap();

        println!("biscuit1 (authority): {}", biscuit1.print());

        let mut block2 = biscuit1.create_block();

        block2.resource_prefix("/folder1/");
        block2.check_right("read");

        let keypair2 = KeyPair::new(&mut rng);
        let biscuit2 = biscuit1
            .append(&mut rng, &keypair2, block2)
            .unwrap();

        //println!("biscuit2:\n{:#?}", biscuit2);
        //panic!();
        {
            let mut verifier = biscuit2.verify(root.public()).unwrap();
            verifier.add_resource("/folder1/file1");
            verifier.add_operation("read");

            let res = verifier.verify();
            println!("res1: {:?}", res);
            res.unwrap();
        }

        let _serialized = biscuit2.to_vec().unwrap();
        //println!("biscuit2 serialized ({} bytes):\n{}", serialized.len(), serialized.to_hex(16));

        let secret = b"secret key";
        let sealed = biscuit2.seal(&secret[..]).unwrap();
        //println!("biscuit2 sealed ({} bytes):\n{}", sealed.len(), sealed.to_hex(16));

        let biscuit3 = Biscuit::from_sealed(&sealed, &secret[..]).unwrap();

        {
            let mut verifier = biscuit3.verify_sealed().unwrap();
            verifier.add_resource("/folder1/file1");
            verifier.add_operation("read");

            let res = verifier.verify();
            println!("res1: {:?}", res);
            res.unwrap();
        }
    }

    #[test]
    fn verif_no_blocks() {
      use crate::token::builder::*;

      let mut rng: StdRng = SeedableRng::seed_from_u64(1234);
      let root = KeyPair::new(&mut rng);

      let mut builder = Biscuit::builder(&root);

      builder.add_authority_fact(fact("right", &[s("authority"), string("file1"), s("read")])).unwrap();
      builder.add_authority_fact(fact("right", &[s("authority"), string("file2"), s("read")])).unwrap();
      builder.add_authority_fact(fact("right", &[s("authority"), string("file1"), s("write")])).unwrap();

      let biscuit1 = builder.build(&mut rng).unwrap();
      println!("{}", biscuit1.print());

      let mut v = biscuit1.verify(root.public()).expect("omg verifier");

      v.add_caveat(rule("right",
          &[s("right")],
          &[pred("right", &[s("authority"), string("file2"), s("write")])]
      )).unwrap();

      //assert!(v.verify().is_err());
      let res = v.verify();
      println!("res: {:?}", res);
      assert_eq!(res,
        Err(Token::FailedLogic(Logic::FailedCaveats(vec![
          FailedCaveat::Verifier(FailedVerifierCaveat { caveat_id: 0, rule: String::from("*right(#right) <- right(#authority, \"file2\", #write)") }),
      ]))));
    }

    #[test]
    fn verifier_queries() {
        let mut rng: StdRng = SeedableRng::seed_from_u64(0);
        let root = KeyPair::new(&mut rng);

        let mut builder = Biscuit::builder(&root);

        builder.add_right("file1", "read");
        builder.add_right("file2", "read");

        let biscuit1 = builder.build(&mut rng).unwrap();

        println!("biscuit1 (authority): {}", biscuit1.print());

        let mut block2 = biscuit1.create_block();

        block2.expiration_date(SystemTime::now() + Duration::from_secs(30));
        block2.revocation_id(1234);

        let keypair2 = KeyPair::new(&mut rng);
        let biscuit2 = biscuit1
            .append(&mut rng, &keypair2, block2)
            .unwrap();

        let mut block3 = biscuit2.create_block();

        block3.expiration_date(SystemTime::now() + Duration::from_secs(10));
        block3.revocation_id(5678);

        let keypair3 = KeyPair::new(&mut rng);
        let biscuit3 = biscuit2
            .append(&mut rng, &keypair3, block3)
            .unwrap();
        {
            let mut verifier = biscuit3.verify(root.public()).unwrap();
            verifier.add_resource("file1");
            verifier.add_operation("read");
            verifier.set_time();

            let res = verifier.verify();
            println!("res1: {:?}", res);

            let res2 = verifier.query(rule(
                "revocation_id_verif",
                &[builder::Atom::Variable("id".to_string())],
                &[pred("revocation_id", &[builder::Atom::Variable("id".to_string())])]
            ));
            println!("res2: {:?}", res2);
            assert_eq!(
              &res2.unwrap().iter().collect::<HashSet<_>>(),
              &[fact("revocation_id_verif", &[int(1234)]), fact("revocation_id_verif", &[int(5678)])].iter().collect::<HashSet<_>>()
            );
        }
    }

    #[test]
    fn caveat_head_name() {
        let mut rng: StdRng = SeedableRng::seed_from_u64(0);
        let root = KeyPair::new(&mut rng);

        let mut builder = Biscuit::builder(&root);

        builder.add_authority_caveat(rule(
            "caveat1",
            &[s("test")],
            &[
                pred("resource", &[s("ambient"), s("hello")]),
            ],
        )).unwrap();

        let biscuit1 = builder.build(&mut rng).unwrap();

        println!("biscuit1 (authority): {}", biscuit1.print());

        // new caveat: can only have read access1
        let mut block2 = biscuit1.create_block();
        block2.add_fact(fact("caveat1", &[s("test")])).unwrap();

        let keypair2 = KeyPair::new(&mut rng);
        let biscuit2 = biscuit1
            .append(&mut rng, &keypair2, block2)
            .unwrap();

        println!("biscuit2: {}", biscuit2.print());

        //println!("generated biscuit token 2: {} bytes\n{}", serialized2.len(), serialized2.to_hex(16));
        {
            let mut verifier = biscuit2.verify(root.public()).unwrap();
            verifier.add_resource("file1");
            verifier.add_operation("read");
            verifier.set_time();

            println!("world:\n{}", verifier.print_world());

            let res = verifier.verify();
            println!("res1: {:?}", res);
            assert_eq!(
              res,
              Err(Token::FailedLogic(Logic::FailedCaveats(vec![
                FailedCaveat::Block(FailedBlockCaveat {
                  block_id: 0,
                  caveat_id: 0,
                  rule: String::from("*caveat1(#test) <- resource(#ambient, #hello)"),
                }),
              ]))));

        }
    }

    #[test]
    fn caveat_requires_fact_in_future_block() {
        let mut rng: StdRng = SeedableRng::seed_from_u64(0);
        let root = KeyPair::new(&mut rng);

        let mut builder = Biscuit::builder(&root);

        builder.add_authority_caveat(rule(
            "requires_name",
            &[var("name")],
            &[
                pred("name", &[var("name")]),
            ],
        )).unwrap();

        let biscuit1 = builder.build(&mut rng).unwrap();

        println!("biscuit1 (authority): {}", biscuit1.print());
        let mut verifier1 = biscuit1.verify(root.public()).unwrap();
        let res1 = verifier1.verify();
        println!("res1: {:?}", res1);
        assert_eq!(
            res1,
            Err(Token::FailedLogic(Logic::FailedCaveats(vec![
                FailedCaveat::Block(FailedBlockCaveat {
                    block_id: 0,
                    caveat_id: 0,
                    rule: String::from("*requires_name($name) <- name($name)"),
                }),
            ]))));

        let mut block2 = biscuit1.create_block();
        block2.add_fact(fact("name", &[s("test")])).unwrap();

        let keypair2 = KeyPair::new(&mut rng);
        let biscuit2 = biscuit1
            .append(&mut rng, &keypair2, block2)
            .unwrap();

        println!("biscuit2 (with name fact): {}", biscuit2.print());
        let mut verifier2 = biscuit2.verify(root.public()).unwrap();
        let res2 = verifier2.verify();
        assert_eq!(res2, Ok(()));
    }

    #[test]
    fn bytes_constraints() {
        let mut rng: StdRng = SeedableRng::seed_from_u64(0);
        let root = KeyPair::new(&mut rng);

        let mut builder = Biscuit::builder(&root);
        builder.add_authority_fact("bytes(#authority, hex:0102AB)").unwrap();
        let biscuit1 = builder.build(&mut rng).unwrap();

        println!("biscuit1 (authority): {}", biscuit1.print());

        let mut block2 = biscuit1.create_block();
        block2.add_rule("*has_bytes(0) <- bytes(#authority, $0) @ $0 in [ hex:00000000, hex:0102AB ]").unwrap();
        let keypair2 = KeyPair::new(&mut rng);
        let biscuit2 = biscuit1
            .append(&mut rng, &keypair2, block2)
            .unwrap();

        let mut verifier = biscuit2.verify(root.public()).unwrap();
        verifier.add_caveat("*ok(0) <- has_bytes($0)").unwrap();

        let res = verifier.verify();
        println!("res1: {:?}", res);
        res.unwrap();

        let res = verifier.query("*data($0) <- bytes(#authority, $0)").unwrap();
        println!("query result: {:x?}", res);
        println!("query result: {}", res[0]);
    }
}
