use std::path::PathBuf;

use asm_lsp::{
    get_comp_resp, get_completes, get_document_symbols, get_goto_def_resp, get_hover_resp,
    get_include_dirs, get_ref_resp, get_sig_help_resp, get_target_config, get_word_from_pos_params,
    instr_filter_targets, populate_directives, populate_instructions,
    populate_name_to_directive_map, populate_name_to_instruction_map,
    populate_name_to_register_map, populate_registers, text_doc_change_to_ts_edit,
    tree_sitter_logger, Arch, Assembler, NameToDirectiveMap, NameToInstructionMap,
    NameToRegisterMap,
};

use lsp_types::notification::{DidChangeTextDocument, DidOpenTextDocument};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, References,
    SignatureHelpRequest,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionOptionsCompletionItem,
    DocumentSymbolResponse, HoverProviderCapability, InitializeParams, OneOf, PositionEncodingKind,
    ServerCapabilities, SignatureHelpOptions, TextDocumentSyncCapability, TextDocumentSyncKind,
    WorkDoneProgressOptions,
};

use anyhow::Result;
use log::{error, info};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_textdocument::FullTextDocument;
use serde_json::json;

// main -------------------------------------------------------------------------------------------
pub fn main() -> Result<()> {
    // initialisation -----------------------------------------------------------------------------
    // Set up logging. Because `stdio_transport` gets a lock on stdout and stdin, we must have our
    // logging only write out to stderr.
    flexi_logger::Logger::try_with_str("info")?.start()?;

    // LSP server initialisation ------------------------------------------------------------------
    info!("Starting asm_lsp...");

    // Create the transport
    let (connection, io_threads) = Connection::stdio();

    // specify UTF-16 encoding for compatibility with lsp-textdocument
    let position_encoding = Some(PositionEncodingKind::UTF16);

    // Run the server and wait for the two threads to end (typically by trigger LSP Exit event).
    let hover_provider = Some(HoverProviderCapability::Simple(true));

    let completion_provider = Some(CompletionOptions {
        completion_item: Some(CompletionOptionsCompletionItem {
            label_details_support: Some(true),
        }),
        trigger_characters: Some(vec![String::from("%"), String::from(".")]),
        ..Default::default()
    });

    let definition_provider = Some(OneOf::Left(true));

    let text_document_sync = Some(TextDocumentSyncCapability::Kind(
        TextDocumentSyncKind::INCREMENTAL,
    ));

    let signature_help_provider = Some(SignatureHelpOptions {
        trigger_characters: None,
        retrigger_characters: None,
        work_done_progress_options: WorkDoneProgressOptions {
            work_done_progress: Some(false),
        },
    });

    let references_provider = Some(OneOf::Left(true));

    let capabilities = ServerCapabilities {
        position_encoding,
        hover_provider,
        completion_provider,
        signature_help_provider,
        definition_provider,
        text_document_sync,
        document_symbol_provider: Some(OneOf::Left(true)),
        references_provider,
        ..ServerCapabilities::default()
    };
    let server_capabilities = serde_json::to_value(capabilities).unwrap();
    let initialization_params = connection.initialize(server_capabilities)?;

    let params: InitializeParams = serde_json::from_value(initialization_params.clone()).unwrap();
    let target_config = get_target_config(&params);

    // create a map of &Instruction_name -> &Instruction - Use that in user queries
    // The Instruction(s) themselves are stored in a vector and we only keep references to the
    // former map
    let x86_instructions = if target_config.instruction_sets.x86 {
        info!("Populating instruction set -> x86...");
        let xml_conts_x86 = include_str!("../../opcodes/x86.xml");
        populate_instructions(xml_conts_x86)?
            .into_iter()
            .map(|instruction| {
                // filter out assemblers by user config
                instr_filter_targets(&instruction, &target_config)
            })
            .filter(|instruction| !instruction.forms.is_empty())
            .collect()
    } else {
        Vec::new()
    };

    let x86_64_instructions = if target_config.instruction_sets.x86_64 {
        info!("Populating instruction set -> x86_64...");
        let xml_conts_x86_64 = include_str!("../../opcodes/x86_64.xml");
        populate_instructions(xml_conts_x86_64)?
            .into_iter()
            .map(|instruction| {
                // filter out assemblers by user config
                instr_filter_targets(&instruction, &target_config)
            })
            .filter(|instruction| !instruction.forms.is_empty())
            .collect()
    } else {
        Vec::new()
    };

    let z80_instructions = if target_config.instruction_sets.z80 {
        info!("Populating instruction set -> z80...");
        let xml_conts_z80 = include_str!("../../opcodes/z80.xml");
        populate_instructions(xml_conts_z80)?
            .into_iter()
            .map(|instruction| {
                // filter out assemblers by user config
                instr_filter_targets(&instruction, &target_config)
            })
            .filter(|instruction| !instruction.forms.is_empty())
            .collect()
    } else {
        Vec::new()
    };

    let mut names_to_instructions = NameToInstructionMap::new();
    populate_name_to_instruction_map(Arch::X86, &x86_instructions, &mut names_to_instructions);
    populate_name_to_instruction_map(
        Arch::X86_64,
        &x86_64_instructions,
        &mut names_to_instructions,
    );
    populate_name_to_instruction_map(Arch::Z80, &z80_instructions, &mut names_to_instructions);

    // create a map of &Register_name -> &Register - Use that in user queries
    // The Register(s) themselves are stored in a vector and we only keep references to the
    // former map
    let x86_registers = if target_config.instruction_sets.x86 {
        info!("Populating register set -> x86...");
        let xml_conts_regs_x86 = include_str!("../../registers/x86.xml");
        populate_registers(xml_conts_regs_x86)?
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };

    let x86_64_registers = if target_config.instruction_sets.x86_64 {
        info!("Populating register set -> x86_64...");
        let xml_conts_regs_x86_64 = include_str!("../../registers/x86_64.xml");
        populate_registers(xml_conts_regs_x86_64)?
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };

    let z80_registers = if target_config.instruction_sets.z80 {
        info!("Populating register set -> z80...");
        let xml_conts_regs_z80 = include_str!("../../registers/z80.xml");
        populate_registers(xml_conts_regs_z80)?
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };

    let mut names_to_registers = NameToRegisterMap::new();
    populate_name_to_register_map(Arch::X86, &x86_registers, &mut names_to_registers);
    populate_name_to_register_map(Arch::X86_64, &x86_64_registers, &mut names_to_registers);
    populate_name_to_register_map(Arch::Z80, &z80_registers, &mut names_to_registers);

    let gas_directives = if target_config.assemblers.gas {
        info!("Populating directive set -> Gas...");
        let xml_conts_gas = include_str!("../../directives/gas_directives.xml");
        populate_directives(xml_conts_gas)?.into_iter().collect()
    } else {
        Vec::new()
    };

    let mut names_to_directives = NameToDirectiveMap::new();
    populate_name_to_directive_map(Assembler::Gas, &gas_directives, &mut names_to_directives);

    let instr_completion_items =
        get_completes(&names_to_instructions, Some(CompletionItemKind::OPERATOR));
    let reg_completion_items =
        get_completes(&names_to_registers, Some(CompletionItemKind::VARIABLE));
    let directive_completion_items =
        get_completes(&names_to_directives, Some(CompletionItemKind::OPERATOR));

    let include_dirs = get_include_dirs();

    main_loop(
        &connection,
        initialization_params,
        &names_to_instructions,
        &names_to_directives,
        &names_to_registers,
        &instr_completion_items,
        &directive_completion_items,
        &reg_completion_items,
        &include_dirs,
    )?;
    io_threads.join()?;

    // Shut down gracefully.
    info!("Shutting down asm_lsp");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn main_loop(
    connection: &Connection,
    params: serde_json::Value,
    names_to_instructions: &NameToInstructionMap,
    names_to_directives: &NameToDirectiveMap,
    names_to_registers: &NameToRegisterMap,
    instruction_completion_items: &[CompletionItem],
    directive_completion_items: &[CompletionItem],
    register_completion_items: &[CompletionItem],
    include_dirs: &[PathBuf],
) -> Result<()> {
    let _params: InitializeParams = serde_json::from_value(params).unwrap();
    let mut curr_doc: Option<FullTextDocument> = None;
    let mut parser = tree_sitter::Parser::new();
    let mut tree: Option<tree_sitter::Tree> = None;
    parser.set_logger(Some(Box::new(tree_sitter_logger)));
    parser.set_language(tree_sitter_asm::language())?;

    let mut empty_resp = Response {
        id: RequestId::from(0),
        result: Some(json!("")),
        error: None,
    };

    info!("Starting asm_lsp loop...");
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                } else if let Ok((id, params)) = cast_req::<HoverRequest>(req.clone()) {
                    // HoverRequest ---------------------------------------------------------------
                    let (word, file_word) = if let Some(ref doc) = curr_doc {
                        (
                            // get the word under the cursor
                            get_word_from_pos_params(
                                doc,
                                &params.text_document_position_params,
                                "",
                            ),
                            // treat the word under the cursor as a filename and grab it as well
                            get_word_from_pos_params(
                                doc,
                                &params.text_document_position_params,
                                ".",
                            ),
                        )
                    } else {
                        continue;
                    };

                    // get documentation ------------------------------------------------------
                    // format response
                    let hover_resp = get_hover_resp(
                        word,
                        file_word,
                        names_to_instructions,
                        names_to_registers,
                        names_to_directives,
                        include_dirs,
                    );
                    if hover_resp.is_some() {
                        let result = serde_json::to_value(&hover_resp).unwrap();
                        let result = Response {
                            id: id.clone(),
                            result: Some(result),
                            error: None,
                        };
                        connection.sender.send(Message::Response(result))?;
                    } else {
                        empty_resp.id = id.clone();
                        connection
                            .sender
                            .send(Message::Response(empty_resp.clone()))?;
                    }
                } else if let Ok((id, params)) = cast_req::<Completion>(req.clone()) {
                    // CompletionRequest ---------------------------------------------------------------
                    // get suggestions ------------------------------------------------------
                    if let Some(ref doc) = curr_doc {
                        let comp_resp = get_comp_resp(
                            doc.get_content(None),
                            &mut parser,
                            &mut tree,
                            &params,
                            instruction_completion_items,
                            directive_completion_items,
                            register_completion_items,
                        );
                        if comp_resp.is_some() {
                            let result = serde_json::to_value(&comp_resp).unwrap();
                            let result = Response {
                                id: id.clone(),
                                result: Some(result),
                                error: None,
                            };
                            connection.sender.send(Message::Response(result))?;
                            continue;
                        }
                    }
                    empty_resp.id = id.clone();
                    connection
                        .sender
                        .send(Message::Response(empty_resp.clone()))?;
                } else if let Ok((id, params)) = cast_req::<GotoDefinition>(req.clone()) {
                    if let Some(ref doc) = curr_doc {
                        let def_resp = get_goto_def_resp(doc, &mut parser, &mut tree, &params);
                        if def_resp.is_some() {
                            let result = serde_json::to_value(&def_resp).unwrap();
                            let result = Response {
                                id: id.clone(),
                                result: Some(result),
                                error: None,
                            };
                            connection.sender.send(Message::Response(result))?;
                        } else {
                            empty_resp.id = id.clone();
                            connection
                                .sender
                                .send(Message::Response(empty_resp.clone()))?;
                        }
                    }
                } else if let Ok((id, params)) = cast_req::<DocumentSymbolRequest>(req.clone()) {
                    // DocumentSymbolRequest ---------------------------------------------------------------
                    if let Some(ref doc) = curr_doc {
                        let symbols = get_document_symbols(
                            doc.get_content(None),
                            &mut parser,
                            &mut tree,
                            &params,
                        );
                        if let Some(symbols) = symbols {
                            let resp = DocumentSymbolResponse::Nested(symbols);
                            let result = serde_json::to_value(&resp).unwrap();
                            let result = Response {
                                id: id.clone(),
                                result: Some(result),
                                error: None,
                            };
                            connection.sender.send(Message::Response(result))?;
                            continue;
                        }
                    }
                    empty_resp.id = id.clone();
                    connection
                        .sender
                        .send(Message::Response(empty_resp.clone()))?;
                } else if let Ok((id, params)) = cast_req::<SignatureHelpRequest>(req.clone()) {
                    // SignatureHelp ---------------------------------------------------------------
                    if let Some(ref doc) = curr_doc {
                        let sig_resp = get_sig_help_resp(
                            doc.get_content(None),
                            &mut parser,
                            &params,
                            &mut tree,
                            names_to_instructions,
                        );

                        if let Some(sig) = sig_resp {
                            let result = serde_json::to_value(&sig).unwrap();

                            let result = Response {
                                id: id.clone(),
                                result: Some(result),
                                error: None,
                            };
                            connection.sender.send(Message::Response(result.clone()))?;
                            continue;
                        }
                    }
                    empty_resp.id = id.clone();
                    connection
                        .sender
                        .send(Message::Response(empty_resp.clone()))?;
                } else if let Ok((id, params)) = cast_req::<References>(req.clone()) {
                    if let Some(ref doc) = curr_doc {
                        let ref_resp = get_ref_resp(doc, &mut parser, &mut tree, &params);
                        if !ref_resp.is_empty() {
                            let result = serde_json::to_value(&ref_resp).unwrap();

                            let result = Response {
                                id: id.clone(),
                                result: Some(result),
                                error: None,
                            };
                            connection.sender.send(Message::Response(result.clone()))?;
                            continue;
                        }
                    }
                    empty_resp.id = id.clone();
                    connection
                        .sender
                        .send(Message::Response(empty_resp.clone()))?;
                } else {
                    error!("Invalid request format -> {:#?}", req);
                }
            }
            Message::Notification(notif) => {
                if let Ok(params) = cast_notif::<DidOpenTextDocument>(notif.clone()) {
                    curr_doc = Some(FullTextDocument::new(
                        params.text_document.language_id.clone(),
                        params.text_document.version,
                        params.text_document.text.clone(),
                    ));
                    tree = parser.parse(params.text_document.text, None);
                } else if let Ok(params) = cast_notif::<DidChangeTextDocument>(notif.clone()) {
                    if let Some(ref mut doc) = curr_doc {
                        // Sync our in-memory copy of the current buffer
                        doc.update(&params.content_changes, params.text_document.version);
                        // Update the TS tree
                        if let Some(ref mut curr_tree) = tree {
                            for change in &params.content_changes {
                                match text_doc_change_to_ts_edit(change, doc) {
                                    Ok(edit) => {
                                        curr_tree.edit(&edit);
                                    }
                                    Err(e) => {
                                        error!("Bad edit info, failed to edit tree - Error: {e}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Message::Response(_resp) => {}
        }
    }
    Ok(())
}

fn cast_req<R>(req: Request) -> Result<(RequestId, R::Params)>
where
    R: lsp_types::request::Request,
    R::Params: serde::de::DeserializeOwned,
{
    match req.extract(R::METHOD) {
        Ok(value) => Ok(value),
        // Fixme please
        Err(e) => Err(anyhow::anyhow!("Error: {e}")),
    }
}

fn cast_notif<R>(notif: Notification) -> Result<R::Params>
where
    R: lsp_types::notification::Notification,
    R::Params: serde::de::DeserializeOwned,
{
    match notif.extract(R::METHOD) {
        Ok(value) => Ok(value),
        // Fixme please
        Err(e) => Err(anyhow::anyhow!("Error: {e}")),
    }
}
