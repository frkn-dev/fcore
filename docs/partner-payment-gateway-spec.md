# Payment Gateway integration for Partner Dashboard

Partner Dashboard (`src/bin/dashboard`) создаёт промокоды партнёров в своей БД (`dashboard.partner_promocodes`). Чтобы эти промокоды реально работали при оплате, Payment Gateway должен уметь принимать их и учитывать в аналитике.

## 1. Создание партнёрского промокода

```http
POST /api/partner/promocodes
Authorization: Bearer <payment-admin-token>
Content-Type: application/json
```

**Request body:**

```json
{
  "code": "PROMO2024",
  "discount_percent": 10,
  "max_uses": 100,
  "duration_days": 30,
  "expires_at": "2026-12-31T23:59:59Z",
  "partner_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

| Поле | Тип | Обязательное | Описание |
|------|-----|--------------|----------|
| `code` | string | да | Уникальный код промокода (uppercase) |
| `discount_percent` | integer | да | Скидка в процентах (0–100) |
| `max_uses` | integer | нет | Максимальное количество использований |
| `duration_days` | integer | нет | Длительность подписки в днях, если применимо |
| `expires_at` | ISO 8601 | нет | Срок действия промокода |
| `partner_id` | UUID | да | ID партнёра в dashboard |

**Response 201 Created:**

```json
{
  "id": "660e8400-e29b-41d4-a716-446655440001"
}
```

`id` — UUID созданного промокода в БД payment-gateway. Dashboard сохранит его в `dashboard.partner_promocodes.payment_promocode_id`.

**Ошибки:**
- `409 Conflict` — код уже существует.
- `400 Bad Request` — невалидные параметры.
- `401 Unauthorized` — неверный/отсутствует токен.

## 2. Удаление (деактивация) партнёрского промокода

```http
DELETE /api/partner/promocodes/{payment_promocode_id}
Authorization: Bearer <payment-admin-token>
```

**Response:** `204 No Content` или `200 OK`.

Если промокод уже удалён — возвращай `404 Not Found`, dashboard обработает это как успех.

## 3. Требования к таблице promocodes в payment-gateway

Добавить колонку `partner_id` (nullable UUID) в таблицу `promocodes`:

```sql
ALTER TABLE promocodes ADD COLUMN IF NOT EXISTS partner_id UUID;
CREATE INDEX IF NOT EXISTS idx_promocodes_partner_id ON promocodes(partner_id);
```

Это нужно для:
- Уникальности кода в рамках всех промокодов.
- Возможности фильтровать/группировать аналитику по партнёру.

## 4. Аналитика по партнёрским промокодам

Уже реализовано: endpoint `/analytics/sales` возвращает `byPromocode`. Dashboard фильтрует его по кодам партнёра.

Дополнительно желательно:
- В `byPromocode` включать все коды, которые есть в БД payment-gateway, даже если по ним не было продаж (count=0, revenue=0).
- Включать `partner_id` в внутренние логи/таблицы транзакций для удобства отладки.

## 5. Авторизация

Для административных операций (создание/удаление промокодов) использовать отдельный `payment-admin-token`. В конфиге dashboard это поле `payment.admin_token`:

```toml
[payment]
endpoint = "http://127.0.0.1:3006"
analytics_token = "your-analytics-token"
admin_token = "your-payment-admin-token"
```

`analytics_token` используется только для чтения `/analytics/sales`.

## 6. Поведение dashboard

- При создании промокода сначала вставляется запись в `dashboard.partner_promocodes`, затем вызывается Payment Gateway.
- Если Payment Gateway отказывает — dashboard удаляет локальную запись и возвращает ошибку партнёру.
- Если Payment Gateway создал промокод, но обновление `payment_promocode_id` в dashboard не удалось — ошибка логируется, промокод остаётся рабочим в payment-gateway.
- При удалении сначала удаляется промокод в Payment Gateway, затем из dashboard.

## 7. Definition of Done

- [ ] Реализован `POST /api/partner/promocodes`.
- [ ] Реализован `DELETE /api/partner/promocodes/{id}`.
- [ ] Добавлена колонка `partner_id` в таблицу `promocodes`.
- [ ] Промокоды, созданные через partner dashboard, работают на странице оплаты.
- [ ] Продажи по ним попадают в `/analytics/sales?granularity=daily` в секцию `byPromocode`.
