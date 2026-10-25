//! Ferramenta `diagnosticar_pc` para o cérebro (LLM).
//!
//! Uso pelo cérebro:
//!
//! ```ignore
//! let def = diagnostics::tool::definition(); // vai na lista de ferramentas da API
//! // quando o modelo chamar a ferramenta com nome == tool::NAME:
//! let resultado = diagnostics::tool::call(&diag, &input); // serde_json::Value
//! ```
//!
//! A chamada bloqueia até a análise terminar (normalmente poucos segundos), então rode-a fora
//! da thread da interface. Um relatório com menos de [`MAX_AGE`] é reaproveitado, a não ser que
//! o modelo peça `atualizar: true`.

use std::time::Duration;

use serde_json::{json, Value};

use super::{now_unix, Category, Diagnostics};

pub const NAME: &str = "diagnosticar_pc";
pub const MAX_AGE: Duration = Duration::from_secs(10 * 60);
pub const TIMEOUT: Duration = Duration::from_secs(90);

/// Definição no formato de ferramenta da API do Claude (`name`, `description`, `input_schema`).
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "description": "Faz um diagnóstico profundo do PC do usuário (Windows) e diz o que está errado: \
            erros do Log de Eventos (telas azuis, desligamentos inesperados, erros de hardware WHEA, \
            erros de disco, travamentos do driver de vídeo, programas que fecham sozinhos), saúde dos \
            discos (SMART, desgaste do SSD, temperatura, espaço livre), dispositivos com problema e \
            drivers antigos, e todas as temperaturas que der para ler. Use quando o usuário perguntar \
            se há algo errado com o PC, por que ele travou, reiniciou, está lento ou quente. Cada \
            problema vem com gravidade (critico, aviso, info) e uma sugestão do que fazer.",
        "input_schema": {
            "type": "object",
            "properties": {
                "categorias": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["eventos", "discos", "drivers", "temperaturas", "sistema"] },
                    "description": "Limita a resposta a estas áreas. Omitir para todas."
                },
                "atualizar": {
                    "type": "boolean",
                    "description": "true força uma análise nova em vez de usar a dos últimos 10 minutos."
                }
            }
        }
    })
}

/// Executa a ferramenta e devolve o resultado em JSON para mandar de volta ao modelo.
pub fn call(diag: &Diagnostics, input: &Value) -> Value {
    let refresh = input.get("atualizar").and_then(Value::as_bool).unwrap_or(false);
    let filter: Option<Vec<Category>> = input
        .get("categorias")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).filter_map(Category::from_name).collect())
        .filter(|v: &Vec<Category>| !v.is_empty());

    let max_age = if refresh { Duration::ZERO } else { MAX_AGE };
    let Some(report) = diag.get_fresh(max_age, TIMEOUT) else {
        return json!({ "erro": "O diagnóstico não terminou a tempo. Tente de novo em instantes." });
    };
    let wants = |c: Category| filter.as_ref().is_none_or(|f| f.contains(&c));

    let findings: Vec<_> = report.findings.iter().filter(|f| wants(f.category)).collect();
    let mut out = json!({
        "resumo": report.summary(),
        "analisado_ha_segundos": (now_unix() - report.generated_unix).max(0),
        "problemas": findings,
        "observacoes": report.notes,
    });
    if wants(Category::Temperaturas) {
        out["temperaturas"] = json!(report.sensors);
    }
    out
}
